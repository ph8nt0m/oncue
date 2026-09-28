mod collector;
mod config;
mod git;
mod i18n;
mod model;
mod ui;

use clap::Parser;
use collector::Collector;
use config::Config;
use i18n::{Lang, age};
use model::{Snapshot, State};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::sync::mpsc;
use std::time::Duration;

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
    let collector = Collector::new(&config);

    if args.json {
        println!("{}", serde_json::to_string_pretty(&collector.collect())?);
        return Ok(());
    }
    if args.once {
        print_text(&collector.collect(), lang);
        return Ok(());
    }
    run_tui(
        collector,
        lang,
        Duration::from_secs(config.interval_secs.max(1)),
    )
}

fn run_tui(collector: Collector, lang: Lang, interval: Duration) -> anyhow::Result<()> {
    let (snap_tx, snap_rx) = mpsc::channel::<Snapshot>();
    let (refresh_tx, refresh_rx) = mpsc::channel::<()>();
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

    let mut terminal = ratatui::init();
    let mut app = ui::App::new(lang);
    let result = (|| -> anyhow::Result<()> {
        loop {
            while let Ok(s) = snap_rx.try_recv() {
                app.set_snapshot(s);
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
            println!(
                "  {} {:<8} {:>4}  {}  {}",
                ui::icon(&s.state),
                lang.state(&s.state),
                waited,
                place,
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
