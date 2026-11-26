//! `loom tui`: a small dashboard over the live niri state.
//!
//! The TUI is read-only except for focusing the selected window. It uses the
//! shared `loom-state` event cache, so it does not poll the compositor.

use std::io::{self, stdout};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use loom_state::{State, StateClient};
use niri_ipc::Request;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Tabs, Wrap};
use ratatui::{Frame, Terminal};

const TABS: [&str; 3] = ["Overview", "Windows", "Outputs"];

struct App {
    state: Option<StateClient>,
    tab: usize,
    selected: usize,
    message: String,
}

impl App {
    fn new() -> Self {
        let (state, message) = match StateClient::connect() {
            Ok(state) => (Some(state), String::new()),
            Err(err) => (None, err),
        };
        Self {
            state,
            tab: 0,
            selected: 0,
            message,
        }
    }

    fn snapshot(&self) -> State {
        self.state
            .as_ref()
            .map(StateClient::snapshot)
            .unwrap_or_default()
    }

    fn connected(&self) -> bool {
        self.state.as_ref().is_some_and(StateClient::is_connected)
    }

    fn next_tab(&mut self) {
        self.tab = (self.tab + 1) % TABS.len();
        self.selected = 0;
    }

    fn move_selection(&mut self, delta: isize, len: usize) {
        if len == 0 {
            self.selected = 0;
        } else if delta < 0 {
            self.selected = self.selected.saturating_sub(1);
        } else {
            self.selected = (self.selected + 1).min(len - 1);
        }
    }

    fn focus_selected(&mut self, state: &State) {
        let windows = state.windows.iter().collect::<Vec<_>>();
        let Some(window) = windows.get(self.selected) else {
            return;
        };
        match focus_window(window.id) {
            Ok(()) => self.message = format!("focused window {}", window.id),
            Err(err) => self.message = err,
        }
    }
}

fn focus_window(id: u64) -> Result<(), String> {
    let mut socket =
        niri_ipc::socket::Socket::connect().map_err(|err| format!("niri IPC: {err}"))?;
    match socket.send(Request::Action(niri_ipc::Action::FocusWindow { id })) {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(message)) => Err(message),
        Err(err) => Err(format!("niri IPC: {err}")),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().any(|arg| arg == "--help" || arg == "-h") {
        println!(
            "loom-tui\n\nA terminal dashboard for Loom.\n\n\
             Usage: loom tui [--help] [--version]\n\n\
             Keys: q quit, Tab switch, j/k move, Enter focus window"
        );
        return Ok(());
    }
    if std::env::args().any(|arg| arg == "--version" || arg == "-V") {
        println!("loom-tui {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let mut app = App::new();
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;
    let result = run(&mut terminal, &mut app);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result?;
    Ok(())
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
) -> io::Result<()> {
    loop {
        if let Some(state) = &app.state
            && state.try_recv().is_some()
        {
            // A redraw is the next iteration.
        }
        terminal.draw(|frame| ui(frame, app))?;

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(KeyEvent {
            code, modifiers, ..
        }) = event::read()?
        else {
            continue;
        };
        if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
            return Ok(());
        }
        match code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Tab | KeyCode::Right => app.next_tab(),
            KeyCode::BackTab | KeyCode::Left => {
                app.tab = (app.tab + TABS.len() - 1) % TABS.len();
                app.selected = 0;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                let len = app.snapshot().windows.len();
                app.move_selection(1, len);
            }
            KeyCode::Char('k') | KeyCode::Up => app.move_selection(-1, 1),
            KeyCode::Enter if app.tab == 1 => {
                let state = app.snapshot();
                app.focus_selected(&state);
            }
            _ => {}
        }
    }
}

fn ui(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(1),
        ])
        .split(area);

    let title = Line::from(vec![
        Span::styled(
            " Loom ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  a connected desktop"),
    ]);
    frame.render_widget(Paragraph::new(title), chunks[0]);

    let tabs = Tabs::new(TABS.iter().map(|tab| Line::from(*tab)))
        .select(app.tab)
        .block(Block::default().borders(Borders::ALL))
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(tabs, chunks[1]);

    let state = app.snapshot();
    match app.tab {
        0 => overview(frame, chunks[2], &state),
        1 => windows(frame, chunks[2], &state, app.selected),
        2 => outputs(frame, chunks[2], &state),
        _ => {}
    }

    let status = if app.connected() {
        format!(
            "{}  ·  {} windows  ·  q quit  Tab switch  j/k move  Enter focus",
            app.message,
            state.windows.len()
        )
    } else {
        format!("disconnected: {}", app.message)
    };
    frame.render_widget(
        Paragraph::new(status).style(Style::default().fg(if app.connected() {
            Color::DarkGray
        } else {
            Color::Red
        })),
        chunks[3],
    );
}

fn overview(frame: &mut Frame, area: Rect, state: &State) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(area);

    let mut lines = Vec::new();
    for (name, output) in &state.outputs {
        let active = output
            .logical
            .as_ref()
            .map(|logical| format!("{}x{}", logical.width, logical.height))
            .unwrap_or_else(|| "off".to_owned());
        lines.push(Line::from(vec![
            Span::styled(name.clone(), Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!("  {active}")),
        ]));
        for workspace in state.workspaces_on(name) {
            let marker = if workspace.is_focused {
                "●"
            } else if workspace.is_active {
                "○"
            } else {
                "·"
            };
            let label = workspace
                .name
                .clone()
                .unwrap_or_else(|| workspace.idx.to_string());
            let windows = state.windows_on(workspace.id).len();
            lines.push(Line::from(format!("  {marker} {label}  ({windows})")));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from("no outputs reported yet"));
    }
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title("Outputs and workspaces")
                .borders(Borders::ALL),
        ),
        columns[0],
    );

    let mut windows = Vec::new();
    for window in &state.windows {
        let app = window.app_id.as_deref().unwrap_or("?");
        let title = window.title.as_deref().unwrap_or("untitled");
        windows.push(Line::from(format!(
            "{} {:>4}  {app}: {title}",
            if window.is_focused { "»" } else { " " },
            window.id
        )));
    }
    frame.render_widget(
        Paragraph::new(windows)
            .wrap(Wrap { trim: true })
            .block(Block::default().title("Windows").borders(Borders::ALL)),
        columns[1],
    );
}

fn windows(frame: &mut Frame, area: Rect, state: &State, selected: usize) {
    let items = state
        .windows
        .iter()
        .map(|window| {
            let app = window.app_id.as_deref().unwrap_or("?");
            let title = window.title.as_deref().unwrap_or("untitled");
            let ws = window
                .workspace_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "-".to_owned());
            let marker = if window.is_focused { "●" } else { " " };
            ListItem::new(format!(
                "{marker} {:>4}  ws {ws:<4}  {app}: {title}",
                window.id
            ))
        })
        .collect::<Vec<_>>();
    let list = List::new(items)
        .block(Block::default().title("Windows").borders(Borders::ALL))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    let mut list_state = ratatui::widgets::ListState::default();
    if !state.windows.is_empty() {
        list_state.select(Some(selected.min(state.windows.len() - 1)));
    }
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn outputs(frame: &mut Frame, area: Rect, state: &State) {
    let mut lines = Vec::new();
    for (name, output) in &state.outputs {
        lines.push(Line::from(vec![
            Span::styled(name.clone(), Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!(
                "  {}  hdr_supported={} hdr_active={}",
                output.make, output.hdr_supported, output.hdr_active
            )),
        ]));
        if let Some(logical) = &output.logical {
            lines.push(Line::from(format!(
                "  {}x{} at {},{} scale {}",
                logical.width, logical.height, logical.x, logical.y, logical.scale
            )));
        }
        if let Some(mode) = output
            .current_mode
            .and_then(|index| output.modes.get(index))
        {
            lines.push(Line::from(format!(
                "  mode {}x{} @ {:.2} Hz",
                mode.width,
                mode.height,
                mode.refresh_rate as f64 / 1000.
            )));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from("no outputs"));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().title("Outputs").borders(Borders::ALL))
            .wrap(Wrap { trim: true }),
        area,
    );
}

/// Renders one frame into a test backend. Kept small and public for the tests.
#[cfg(test)]
fn render_text(app: &App, width: u16, height: u16) -> String {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| ui(frame, app)).unwrap();
    let buffer = terminal.backend().buffer();
    buffer
        .content()
        .chunks(width as usize)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_cycle_and_selection_is_clamped() {
        let mut app = App {
            state: None,
            tab: 0,
            selected: 0,
            message: String::new(),
        };
        app.next_tab();
        assert_eq!(app.tab, 1);
        app.move_selection(1, 2);
        assert_eq!(app.selected, 1);
        app.move_selection(1, 2);
        assert_eq!(app.selected, 1);
        app.move_selection(-1, 2);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn disconnected_dashboard_still_renders() {
        let app = App {
            state: None,
            tab: 0,
            selected: 0,
            message: "test".to_owned(),
        };
        let text = render_text(&app, 80, 20);
        assert!(text.contains("Loom"));
        assert!(text.contains("disconnected"));
    }

    #[test]
    fn focus_window_uses_a_direct_request() {
        // The action is constructed directly; no compositor is required here.
        let action = niri_ipc::Action::FocusWindow { id: 9 };
        assert_eq!(
            serde_json::to_string(&Request::Action(action)).unwrap(),
            "{\"Action\":{\"FocusWindow\":{\"id\":9}}}"
        );
    }
}
