use crate::i18n::{Lang, age};
use crate::model::{Attention, Session, Snapshot, State};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState, Wrap};

pub struct App {
    pub snapshot: Option<Snapshot>,
    pub lang: Lang,
    pub show_dormant: bool,
    /// Selected session key, kept stable across refreshes.
    pub selected: Option<String>,
}

impl App {
    pub fn new(lang: Lang) -> Self {
        Self {
            snapshot: None,
            lang,
            show_dormant: false,
            selected: None,
        }
    }

    fn queue(&self) -> Vec<&Session> {
        self.visible(|s| matches!(s, State::NeedsYou(_)))
    }

    fn rest(&self) -> Vec<&Session> {
        let show_dormant = self.show_dormant;
        self.visible(move |s| match s {
            State::Working => true,
            State::Dormant => show_dormant,
            State::NeedsYou(_) => false,
        })
    }

    fn visible(&self, pred: impl Fn(&State) -> bool) -> Vec<&Session> {
        self.snapshot
            .iter()
            .flat_map(|s| s.sessions.iter())
            .filter(|s| pred(&s.state))
            .collect()
    }

    fn order(&self) -> Vec<&Session> {
        let mut all = self.queue();
        all.extend(self.rest());
        all
    }

    pub fn set_snapshot(&mut self, snapshot: Snapshot) {
        self.snapshot = Some(snapshot);
        let order = self.order();
        let still_there = self
            .selected
            .as_ref()
            .is_some_and(|k| order.iter().any(|s| &s.key == k));
        if !still_there {
            self.selected = order.first().map(|s| s.key.clone());
        }
    }

    pub fn move_by(&mut self, delta: isize) {
        let order = self.order();
        if order.is_empty() {
            return;
        }
        let cur = self
            .selected
            .as_ref()
            .and_then(|k| order.iter().position(|s| &s.key == k))
            .unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, order.len() as isize - 1) as usize;
        self.selected = Some(order[next].key.clone());
    }

    pub fn toggle_dormant(&mut self) {
        self.show_dormant = !self.show_dormant;
        if let Some(s) = self.snapshot.take() {
            self.set_snapshot(s);
        }
    }

    fn selected_session(&self) -> Option<&Session> {
        let key = self.selected.as_ref()?;
        self.snapshot
            .as_ref()?
            .sessions
            .iter()
            .find(|s| &s.key == key)
    }
}

pub fn draw(f: &mut Frame, app: &App) {
    let t = app.lang.text();
    let queue = app.queue();
    let rest = app.rest();
    let queue_height = (queue.len().max(1) as u16 + 2).min(f.area().height / 2);
    let [header, queue_area, rest_area, detail_area, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(queue_height),
        Constraint::Min(3),
        Constraint::Length(10),
        Constraint::Length(1),
    ])
    .areas(f.area());

    draw_header(f, header, app);

    let queue_block = Block::bordered()
        .title(format!(" {} ({}) ", t.needs_you, queue.len()))
        .border_style(Style::new().fg(if queue.is_empty() {
            Color::DarkGray
        } else {
            Color::Yellow
        }));
    if queue.is_empty() {
        f.render_widget(
            Paragraph::new(t.nothing_waiting)
                .style(Style::new().fg(Color::DarkGray))
                .block(queue_block),
            queue_area,
        );
    } else {
        draw_table(f, queue_area, app, &queue, queue_block);
    }

    let rest_title = if app.show_dormant {
        format!(" {} · {} ({}) ", t.working, t.dormant, rest.len())
    } else {
        format!(" {} ({}) ", t.working, rest.len())
    };
    let rest_block = Block::bordered()
        .title(rest_title)
        .border_style(Style::new().fg(Color::DarkGray));
    draw_table(f, rest_area, app, &rest, rest_block);

    draw_detail(f, detail_area, app);

    let warning = app
        .snapshot
        .as_ref()
        .and_then(|s| s.warnings.first())
        .map(|w| format!("  ⚠ {w}"))
        .unwrap_or_default();
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(t.help, Style::new().fg(Color::DarkGray)),
            Span::styled(warning, Style::new().fg(Color::Yellow)),
        ])),
        footer,
    );
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let t = app.lang.text();
    let (needs, working, dormant) = app.snapshot.as_ref().map_or((0, 0, 0), |s| {
        (
            s.count(|st| matches!(st, State::NeedsYou(_))),
            s.count(|st| *st == State::Working),
            s.count(|st| *st == State::Dormant),
        )
    });
    let clock = chrono::Local::now().format("%H:%M:%S").to_string();
    let line = Line::from(vec![
        Span::styled(
            " oncue ",
            Style::new()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!("{} {needs}", t.needs_you),
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" · "),
        Span::styled(
            format!("{} {working}", t.working),
            Style::new().fg(Color::Green),
        ),
        Span::raw(" · "),
        Span::styled(
            format!("{} {dormant}", t.dormant),
            Style::new().fg(Color::DarkGray),
        ),
        Span::raw("   "),
        Span::styled(clock, Style::new().fg(Color::DarkGray)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn draw_table(f: &mut Frame, area: Rect, app: &App, rows: &[&Session], block: Block) {
    let now = app.snapshot.as_ref().map_or(0, |s| s.generated_at_ms);
    let cells: Vec<[String; 5]> = rows
        .iter()
        .map(|s| {
            let waited = s
                .since_ms
                .map(|t| age(now.saturating_sub(t)))
                .unwrap_or_default();
            let place = match &s.branch {
                Some(b) if b != "HEAD" => format!("{} ({b})", s.project),
                _ => s.project.clone(),
            };
            let pr = s
                .prs
                .last()
                .map(|p| {
                    format!(
                        "{}#{}",
                        p.repo.rsplit('/').next().unwrap_or(&p.repo),
                        p.number
                    )
                })
                .unwrap_or_default();
            let what = match (&s.state, &s.activity) {
                (State::Working, Some(a)) => format!("{}  — {a}", s.title),
                _ => s.title.clone(),
            };
            [
                app.lang.state(&s.state).to_string(),
                waited,
                place,
                pr,
                what,
            ]
        })
        .collect();
    // Size the place and PR columns to their content so the title gets the rest.
    let width = |i: usize, cap: u16| {
        cells
            .iter()
            .map(|c| Line::from(c[i].as_str()).width() as u16)
            .max()
            .unwrap_or(0)
            .min(cap)
    };
    let widths = [
        Constraint::Length(1),
        Constraint::Length(width(0, 8).max(4)),
        Constraint::Length(4),
        Constraint::Length(width(2, 32)),
        Constraint::Length(width(3, 24)),
        Constraint::Fill(1),
    ];
    let table_rows = rows
        .iter()
        .zip(cells)
        .map(|(s, [label, waited, place, pr, what])| {
            let color = state_color(&s.state);
            Row::new(vec![
                Cell::from(icon(&s.state)).style(Style::new().fg(color)),
                Cell::from(label).style(Style::new().fg(color)),
                Cell::from(waited),
                Cell::from(place).style(Style::new().fg(Color::Cyan)),
                Cell::from(pr).style(Style::new().fg(Color::Blue)),
                Cell::from(what),
            ])
        });
    let table = Table::new(table_rows, widths)
        .block(block)
        .column_spacing(1)
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));

    let mut state = TableState::default();
    state.select(
        app.selected
            .as_ref()
            .and_then(|k| rows.iter().position(|s| &s.key == k)),
    );
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_detail(f: &mut Frame, area: Rect, app: &App) {
    let t = app.lang.text();
    let block = Block::bordered()
        .title(format!(" {} ", t.detail))
        .border_style(Style::new().fg(Color::DarkGray));
    let Some(s) = app.selected_session() else {
        f.render_widget(block, area);
        return;
    };
    let mut lines = vec![
        Line::from(Span::styled(
            s.title.clone(),
            Style::new().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!(
                "{} · {} · {}{}",
                s.agent,
                s.cwd.display(),
                s.branch.as_deref().unwrap_or("-"),
                s.paseo_id
                    .as_deref()
                    .map(|id| format!(" · paseo {}", &id[..id.len().min(7)]))
                    .unwrap_or_default(),
            ),
            Style::new().fg(Color::DarkGray),
        )),
    ];
    if let Some(d) = s.detail.as_deref().or(s.activity.as_deref()) {
        lines.push(Line::from(Span::styled(
            d.to_string(),
            Style::new().fg(state_color(&s.state)),
        )));
    }
    if let Some(reset) = s.resets_at_ms {
        let now = app.snapshot.as_ref().map_or(0, |x| x.generated_at_ms);
        let at = chrono::DateTime::from_timestamp_millis(reset as i64)
            .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
            .unwrap_or_default();
        let when = if reset <= now {
            t.reset_passed.to_string()
        } else {
            format!("{} {}", t.reset_in, age(reset - now))
        };
        lines.push(Line::from(format!("{} {at} · {when}", t.resets)));
    }
    if !s.options.is_empty() {
        lines.push(Line::from(format!(
            "{}: {}",
            t.options,
            s.options.join(" | ")
        )));
    }
    if !s.prs.is_empty() {
        let prs: Vec<String> = s
            .prs
            .iter()
            .map(|p| format!("{}#{}", p.repo, p.number))
            .collect();
        lines.push(Line::from(format!("{}: {}", t.prs, prs.join(", "))));
    }
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }).block(block),
        area,
    );
}

pub fn icon(state: &State) -> &'static str {
    match state {
        State::NeedsYou(Attention::Question) => "?",
        State::NeedsYou(Attention::Permission) => "!",
        State::NeedsYou(Attention::Plan) => "≡",
        State::NeedsYou(Attention::Error) => "✗",
        State::NeedsYou(Attention::Limited) => "⏸",
        State::NeedsYou(Attention::Unread) => "✓",
        State::NeedsYou(Attention::Idle) => "·",
        State::Working => "●",
        State::Dormant => "◌",
    }
}

fn state_color(state: &State) -> Color {
    match state {
        State::NeedsYou(Attention::Question) => Color::Magenta,
        State::NeedsYou(Attention::Permission | Attention::Error) => Color::Red,
        State::NeedsYou(Attention::Plan) => Color::Cyan,
        State::NeedsYou(Attention::Limited) => Color::Blue,
        State::NeedsYou(Attention::Unread) => Color::Yellow,
        State::NeedsYou(Attention::Idle) => Color::Gray,
        State::Working => Color::Green,
        State::Dormant => Color::DarkGray,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Host, PrLink};
    use ratatui::{Terminal, backend::TestBackend};

    fn session(key: &str, state: State, title: &str) -> Session {
        Session {
            key: key.into(),
            agent: "claude".into(),
            host: Host::Paseo,
            title: title.into(),
            cwd: "/work/web-app".into(),
            project: "web-app".into(),
            branch: Some("feat/x".into()),
            state,
            since_ms: Some(1_000),
            detail: Some("어느 호스트로 받을까요?".into()),
            options: vec!["A".into(), "B".into()],
            activity: None,
            resets_at_ms: None,
            pid: Some(1),
            session_id: Some(key.into()),
            paseo_id: Some("a1b2c3d4-e5f6".into()),
            prs: vec![PrLink {
                repo: "o/r".into(),
                number: 7,
                url: String::new(),
            }],
        }
    }

    fn render(app: &App) -> String {
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        // Wide characters leave a blank cell after them; drop spaces so text
        // can be matched regardless of width.
        buf.content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>()
            .replace(' ', "")
    }

    #[test]
    fn renders_queue_before_working_and_selects_first() {
        let mut app = App::new(Lang::Ko);
        app.set_snapshot(Snapshot {
            generated_at_ms: 61_000,
            sessions: vec![
                session("q", State::NeedsYou(Attention::Question), "스레드 설계"),
                session("w", State::Working, "CI 고치기"),
            ],
            warnings: vec![],
        });
        assert_eq!(app.selected.as_deref(), Some("q"));
        let screen = render(&app);
        assert!(screen.contains("질문"));
        assert!(screen.contains("o/r#7"));
        app.move_by(1);
        assert_eq!(app.selected.as_deref(), Some("w"));
        app.move_by(5);
        assert_eq!(app.selected.as_deref(), Some("w"));
    }

    #[test]
    fn empty_queue_says_nothing_is_waiting() {
        let mut app = App::new(Lang::En);
        app.set_snapshot(Snapshot {
            generated_at_ms: 0,
            sessions: vec![],
            warnings: vec![],
        });
        assert!(render(&app).contains("Nothingiswaitingonyou."));
    }
}
