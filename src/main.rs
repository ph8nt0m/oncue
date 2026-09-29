mod actions;
mod collector;
mod config;
mod git;
mod i18n;
mod issues;
mod model;
mod ui;

use clap::Parser;
use collector::Collector;
use config::Config;
use i18n::{Lang, age};
use model::{Attention, Snapshot, State};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::collections::HashSet;
use std::sync::mpsc;
use std::time::Duration;
use std::time::Instant;

/// An attention queue for your AI coding agents.
#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Print one snapshot as text and exit.
    #[arg(long)]
    once: bool,
    /// Print one snapshot as JSON and exit.
    #[arg(long)]
    json: bool,
    /// UI language: en or ko (default: config, then LANG).
    #[arg(long)]
    lang: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let config = Config::load()?;
    let lang = Lang::detect(args.lang.as_deref().unwrap_or(&config.language));
    let collector = Collector::new(&config, !(args.json || args.once));

    if args.json {
        println!("{}", serde_json::to_string_pretty(&collector.collect())?);
        return Ok(());
    }
    if args.once {
        print_text(&collector.collect(), lang);
        return Ok(());
    }
    run_tui(collector, lang, &config)
}

/// Reasons worth a desktop notification: someone has to decide something.
fn notifies(a: Attention) -> bool {
    matches!(
        a,
        Attention::Question
            | Attention::Permission
            | Attention::Plan
            | Attention::Merge
            | Attention::Error
    )
}

/// Startup fills in GitHub and Linear state over the first seconds; those are
/// not new events, so notifications only start after this.
const NOTIFY_WARMUP: Duration = Duration::from_secs(30);

fn run_tui(collector: Collector, lang: Lang, config: &Config) -> anyhow::Result<()> {
    let interval = Duration::from_secs(config.interval_secs.max(1));
    let (snap_tx, snap_rx) = mpsc::channel::<Snapshot>();
    let (refresh_tx, refresh_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        loop {
            if snap_tx.send(collector.collect()).is_err() {
                return;
            }
            match refresh_rx.recv_timeout(interval) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    });

    let t = lang.text();
    let paseo_server = actions::paseo_server_id();
    let started = Instant::now();
    let mut notified: HashSet<(String, Attention)> = HashSet::new();
    let mut terminal = ratatui::init();
    let mut app = ui::App::new(lang);
    let result = (|| -> anyhow::Result<()> {
        loop {
            while let Ok(s) = snap_rx.try_recv() {
                for session in &s.sessions {
                    let Some(a) = session.attention().filter(|a| notifies(*a)) else {
                        continue;
                    };
                    let fresh = notified.insert((session.key.clone(), a));
                    if fresh && config.notify && started.elapsed() > NOTIFY_WARMUP {
                        let title = format!("oncue · {}", lang.attention(a));
                        let body = model::one_line(&session.title, 120);
                        std::thread::spawn(move || actions::notify(&title, &body));
                    }
                }
                app.set_snapshot(s);
            }
            while let Ok(msg) = done_rx.try_recv() {
                app.status = Some(msg);
                let _ = refresh_tx.send(());
            }
            terminal.draw(|f| ui::draw(f, &app))?;
            if !event::poll(Duration::from_millis(250))? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            app.status = None;

            if let Some((action, _)) = app.pending.take() {
                if key.code == KeyCode::Char('y') {
                    let done = done_tx.clone();
                    std::thread::spawn(move || {
                        let msg = match action.run() {
                            Ok(()) => t.sent.to_string(),
                            Err(e) => format!("{}: {e}", t.failed),
                        };
                        let _ = done.send(msg);
                    });
                }
                // Any other key cancels.
                continue;
            }

            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(());
                }
                KeyCode::Char('j') | KeyCode::Down => app.move_by(1),
                KeyCode::Char('k') | KeyCode::Up => app.move_by(-1),
                KeyCode::Char('g') | KeyCode::Home => app.move_by(isize::MIN / 2),
                KeyCode::Char('G') | KeyCode::End => app.move_by(isize::MAX / 2),
                KeyCode::Char('d') => app.toggle_dormant(),
                KeyCode::Char('r') => {
                    let _ = refresh_tx.send(());
                }
                KeyCode::Enter => {
                    let target = app
                        .selected_session()
                        .and_then(|s| actions::open_target(s, paseo_server.as_deref()));
                    match target {
                        Some(target) => {
                            let done = done_tx.clone();
                            std::thread::spawn(move || {
                                let msg = match actions::open(&target) {
                                    Ok(()) => format!("{}: {target}", t.opened),
                                    Err(e) => format!("{}: {e}", t.failed),
                                };
                                let _ = done.send(msg);
                            });
                        }
                        None => app.status = Some(t.nothing_to_open.into()),
                    }
                }
                KeyCode::Char(c @ '1'..='9') => {
                    let idx = c as usize - '1' as usize;
                    let Some(text) = config.replies.get(idx).cloned() else {
                        continue;
                    };
                    let Some(s) = app.selected_session() else {
                        continue;
                    };
                    match s.paseo_id.clone() {
                        Some(paseo_id) => {
                            let question = format!(
                                "{} 「{text}」 → {}?",
                                t.confirm_reply,
                                model::one_line(&s.title, 50)
                            );
                            app.pending =
                                Some((actions::Action::Reply { paseo_id, text }, question));
                        }
                        None => app.status = Some(t.not_paseo.into()),
                    }
                }
                KeyCode::Char('a') => {
                    let Some(s) = app.selected_session() else {
                        continue;
                    };
                    match (s.paseo_id.clone(), s.permit_id.clone()) {
                        (Some(paseo_id), Some(request_id)) => {
                            let what = s.detail.as_deref().unwrap_or("");
                            let question =
                                format!("{} {}?", t.confirm_allow, model::one_line(what, 80));
                            app.pending = Some((
                                actions::Action::Allow {
                                    paseo_id,
                                    request_id,
                                },
                                question,
                            ));
                        }
                        (None, _) => app.status = Some(t.not_paseo.into()),
                        (_, None) => app.status = Some(t.no_permit.into()),
                    }
                }
                _ => {}
            }
        }
    })();
    ratatui::restore();
    result
}

fn print_text(snapshot: &Snapshot, lang: Lang) {
    let t = lang.text();
    let now = snapshot.generated_at_ms;
    if !snapshot.usage.is_empty() {
        let line: String = ui::usage_parts(&snapshot.usage, now)
            .into_iter()
            .map(|(text, _)| text)
            .collect();
        println!("{}", line.trim_start());
    }
    type Section<'a> = (&'a str, fn(&State) -> bool);
    let sections: [Section; 3] = [
        (t.needs_you, |s| matches!(s, State::NeedsYou(_))),
        (t.working, |s| *s == State::Working),
        (t.dormant, |s| *s == State::Dormant),
    ];
    for (title, pred) in sections {
        let rows: Vec<_> = snapshot
            .sessions
            .iter()
            .filter(|s| pred(&s.state))
            .collect();
        println!("{title} ({})", rows.len());
        for s in rows {
            let waited = s
                .since_ms
                .map(|x| age(now.saturating_sub(x)))
                .unwrap_or_default();
            let place = match &s.branch {
                Some(b) => format!("{} ({b})", s.project),
                None => s.project.clone(),
            };
            let issue = s
                .issues
                .first()
                .map(|i| format!(" [{}]", i.key))
                .unwrap_or_default();
            let overlap = if s.overlaps.is_empty() { "" } else { "⚠ " };
            println!(
                "  {} {:<8} {:>4}  {}{}  {}{}",
                ui::icon(&s.state),
                lang.state(&s.state),
                waited,
                place,
                issue,
                overlap,
                model::one_line(&s.title, 80)
            );
            if let Some(d) = s.detail.as_deref().or(s.activity.as_deref()) {
                println!("               → {}", model::one_line(d, 120));
            }
        }
    }
    for w in &snapshot.warnings {
        eprintln!("warning: {w}");
    }
}
