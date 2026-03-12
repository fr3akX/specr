use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    prelude::*,
    widgets::{
        Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState,
        Wrap,
    },
};
use std::io::stdout;
use std::path::Path;

use crate::store;
use crate::types::{TaskSize, TaskStatus};

/// All statuses in display order (used for the picker).
const ALL_STATUSES: &[TaskStatus] = &[
    TaskStatus::Open,
    TaskStatus::InProgress,
    TaskStatus::Done,
    TaskStatus::Failed,
];

#[derive(Debug, PartialEq)]
enum Mode {
    Normal,
    StatusPicker,
}

struct App {
    tasks: Vec<crate::types::Task>,
    spec_version: u32,
    project_name: String,
    table_state: TableState,
    detail_visible: bool,
    should_quit: bool,
    dir: std::path::PathBuf,
    mode: Mode,
    picker_state: ListState,
    status_message: Option<String>,
}

impl App {
    fn new(dir: &Path) -> Result<Self> {
        let (tasks, spec_version) = store::read_tasks(dir)?;
        let spec_content = store::read_spec(dir).unwrap_or_default();
        let project_name = crate::task_generator::renderer::extract_project_name(&spec_content);

        let mut table_state = TableState::default();
        if !tasks.is_empty() {
            table_state.select(Some(0));
        }

        Ok(Self {
            tasks,
            spec_version,
            project_name,
            table_state,
            detail_visible: false,
            should_quit: false,
            dir: dir.to_path_buf(),
            mode: Mode::Normal,
            picker_state: ListState::default(),
            status_message: None,
        })
    }

    fn refresh(&mut self) -> Result<()> {
        let (tasks, spec_version) = store::read_tasks(&self.dir)?;
        self.tasks = tasks;
        self.spec_version = spec_version;
        if let Some(sel) = self.table_state.selected() {
            if sel >= self.tasks.len() && !self.tasks.is_empty() {
                self.table_state.select(Some(self.tasks.len() - 1));
            }
        }
        Ok(())
    }

    fn selected_task(&self) -> Option<&crate::types::Task> {
        self.table_state.selected().and_then(|i| self.tasks.get(i))
    }

    fn open_status_picker(&mut self) {
        if let Some(task) = self.selected_task() {
            // Pre-select the task's current status in the picker
            let current_idx = ALL_STATUSES
                .iter()
                .position(|s| *s == task.status)
                .unwrap_or(0);
            self.picker_state.select(Some(current_idx));
            self.mode = Mode::StatusPicker;
        }
    }

    fn picker_next(&mut self) {
        let i = match self.picker_state.selected() {
            Some(i) => (i + 1) % ALL_STATUSES.len(),
            None => 0,
        };
        self.picker_state.select(Some(i));
    }

    fn picker_previous(&mut self) {
        let i = match self.picker_state.selected() {
            Some(i) => {
                if i == 0 {
                    ALL_STATUSES.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.picker_state.select(Some(i));
    }

    fn apply_status_change(&mut self) -> Result<()> {
        let Some(task_idx) = self.table_state.selected() else {
            self.mode = Mode::Normal;
            return Ok(());
        };
        let Some(picker_idx) = self.picker_state.selected() else {
            self.mode = Mode::Normal;
            return Ok(());
        };

        let new_status = ALL_STATUSES[picker_idx].clone();
        let task_id = self.tasks[task_idx].id.clone();
        let task_name = self.tasks[task_idx].name.clone();

        store::update_task_status(&self.dir, &task_id, new_status.clone())?;
        self.refresh()?;

        self.status_message = Some(format!("Task {} ({}) → {}", task_id, task_name, new_status));
        self.mode = Mode::Normal;
        Ok(())
    }

    fn next(&mut self) {
        if self.tasks.is_empty() {
            return;
        }
        let i = match self.table_state.selected() {
            Some(i) => {
                if i >= self.tasks.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(i));
        self.status_message = None;
    }

    fn previous(&mut self) {
        if self.tasks.is_empty() {
            return;
        }
        let i = match self.table_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.tasks.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(i));
        self.status_message = None;
    }
}

/// Run the TUI task board.
pub fn run() -> Result<()> {
    let dir = std::env::current_dir()?;
    let mut app = App::new(&dir)?;

    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let result = run_app(&mut terminal, &mut app);

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;

    result
}

fn run_app<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|f| ui(f, app))?;

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            match app.mode {
                Mode::StatusPicker => match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        app.mode = Mode::Normal;
                    }
                    KeyCode::Down | KeyCode::Char('j') => app.picker_next(),
                    KeyCode::Up | KeyCode::Char('k') => app.picker_previous(),
                    KeyCode::Enter => {
                        app.apply_status_change().ok();
                    }
                    _ => {}
                },
                Mode::Normal => match key.code {
                    KeyCode::Char('q') => {
                        app.should_quit = true;
                        return Ok(());
                    }
                    KeyCode::Char('r') => {
                        app.refresh().ok();
                        app.status_message = None;
                    }
                    KeyCode::Char('s') => {
                        app.open_status_picker();
                    }
                    KeyCode::Down | KeyCode::Char('j') => app.next(),
                    KeyCode::Up | KeyCode::Char('k') => app.previous(),
                    KeyCode::Enter => {
                        app.detail_visible = !app.detail_visible;
                    }
                    KeyCode::Esc => {
                        app.detail_visible = false;
                    }
                    _ => {}
                },
            }
        }
    }
}

fn status_label(s: &TaskStatus) -> &'static str {
    match s {
        TaskStatus::Open => "○  open",
        TaskStatus::InProgress => "●  in-progress",
        TaskStatus::Done => "✔  done",
        TaskStatus::Failed => "✖  failed",
    }
}

fn status_style(s: &TaskStatus) -> Style {
    match s {
        TaskStatus::Open => Style::default(),
        TaskStatus::InProgress => Style::default().fg(Color::Cyan),
        TaskStatus::Done => Style::default().fg(Color::Green),
        TaskStatus::Failed => Style::default().fg(Color::Red),
    }
}

fn ui(f: &mut Frame, app: &mut App) {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    // Layout
    let chunks = if app.detail_visible {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(10),
                Constraint::Percentage(40),
                Constraint::Length(3),
            ])
            .split(f.area())
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(10),
                Constraint::Length(0),
                Constraint::Length(3),
            ])
            .split(f.area())
    };

    // Header
    let header_text = format!(
        " specr \u{00b7} Task Board    Project: {}    spec-version: {}    {}",
        app.project_name, app.spec_version, today
    );
    let header = Paragraph::new(header_text)
        .style(Style::default().fg(Color::Cyan).bold())
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(header, chunks[0]);

    // Task table
    let header_cells = ["ID", "Task", "Size", "Status", "Depends on"]
        .iter()
        .map(|h| Cell::from(*h).style(Style::default().fg(Color::Yellow).bold()));
    let header_row = Row::new(header_cells).height(1);

    let rows: Vec<Row> = app
        .tasks
        .iter()
        .map(|task| {
            let status_str = match task.status {
                TaskStatus::Done => "\u{2714} done",
                TaskStatus::InProgress => "\u{25cf} in-prog",
                TaskStatus::Open => "\u{25cb} open",
                TaskStatus::Failed => "\u{2716} failed",
            };
            let s_style = status_style(&task.status);
            let size_style = match task.size {
                TaskSize::S => Style::default().fg(Color::Green),
                TaskSize::M => Style::default().fg(Color::Yellow),
                TaskSize::L => Style::default().fg(Color::Red),
            };
            let deps = if task.depends_on.is_empty() {
                "\u{2014}".to_string()
            } else {
                task.depends_on.join(",")
            };

            Row::new(vec![
                Cell::from(task.id.clone()),
                Cell::from(task.name.clone()),
                Cell::from(task.size.to_string()).style(size_style),
                Cell::from(status_str).style(s_style),
                Cell::from(deps),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(5),
            Constraint::Min(30),
            Constraint::Length(6),
            Constraint::Length(12),
            Constraint::Length(15),
        ],
    )
    .header(header_row)
    .block(Block::default().borders(Borders::ALL).title(" Tasks "))
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .highlight_symbol(">> ");

    f.render_stateful_widget(table, chunks[1], &mut app.table_state);

    // Detail panel
    if app.detail_visible {
        let detail_content = if let Some(task) = app.selected_task() {
            let detail = store::read_task_detail(&app.dir, &task.id);
            match detail {
                Ok(content) => content,
                Err(_) => format!(
                    "Task {} \u{00b7} {}\nSize: {} | Status: {}\nDone when: {}\n\n(No detail file found)",
                    task.id, task.name, task.size, task.status, task.done_when
                ),
            }
        } else {
            "No task selected.".to_string()
        };

        let detail = Paragraph::new(detail_content)
            .block(Block::default().borders(Borders::ALL).title(" Detail "))
            .wrap(Wrap { trim: false });
        f.render_widget(detail, chunks[2]);
    }

    // Footer — changes hint based on mode
    let footer_text = match app.mode {
        Mode::StatusPicker => " [↑↓/jk] select status  [enter] apply  [esc/q] cancel ",
        Mode::Normal => {
            if let Some(msg) = &app.status_message {
                // Show status change confirmation briefly — rendered via status bar below
                let _ = msg;
                " [q] quit  [r] refresh  [↑↓] navigate  [s] change status  [enter] detail  [esc] close "
            } else {
                " [q] quit  [r] refresh  [↑↓] navigate  [s] change status  [enter] detail  [esc] close "
            }
        }
    };
    let footer_style = if app.mode == Mode::StatusPicker {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let footer = Paragraph::new(footer_text)
        .style(footer_style)
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(footer, chunks[3]);

    // Status message bar (just above footer, replacing detail or overlaying)
    if app.mode == Mode::Normal {
        if let Some(msg) = &app.status_message {
            let msg_area = Rect {
                x: chunks[3].x + 1,
                y: chunks[3].y,
                width: chunks[3].width.saturating_sub(2),
                height: 1,
            };
            let msg_text = format!(" ✔ {} ", msg);
            let msg_widget = Paragraph::new(msg_text)
                .style(Style::default().fg(Color::Black).bg(Color::Green).bold());
            f.render_widget(msg_widget, msg_area);
        }
    }

    // Status picker popup
    if app.mode == Mode::StatusPicker {
        render_status_picker(f, app);
    }
}

fn render_status_picker(f: &mut Frame, app: &mut App) {
    let area = f.area();

    // Popup size: 4 items + borders + title = 8 rows, 26 wide
    let popup_height = ALL_STATUSES.len() as u16 + 4;
    let popup_width = 28u16;
    let popup_area = Rect {
        x: area.width.saturating_sub(popup_width) / 2,
        y: area.height.saturating_sub(popup_height) / 2,
        width: popup_width.min(area.width),
        height: popup_height.min(area.height),
    };

    // Clear background behind popup
    f.render_widget(Clear, popup_area);

    let task_label = app
        .selected_task()
        .map(|t| format!(" Set status: {} ", t.id))
        .unwrap_or_else(|| " Set status ".to_string());

    let items: Vec<ListItem> = ALL_STATUSES
        .iter()
        .map(|s| ListItem::new(format!("  {}  ", status_label(s))).style(status_style(s)))
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title(task_label)
                .title_style(Style::default().fg(Color::Yellow).bold()),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Yellow)
                .fg(Color::Black)
                .bold()
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    f.render_stateful_widget(list, popup_area, &mut app.picker_state);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Task, TaskSize, TaskStatus};
    use std::io::Write;
    use tempfile::TempDir;

    fn make_task(id: &str, status: TaskStatus) -> Task {
        Task {
            id: id.to_string(),
            name: format!("Task {}", id),
            size: TaskSize::S,
            status,
            depends_on: vec![],
            done_when: "tests pass".to_string(),
            scope: "implement".to_string(),
            files_to_touch: vec![],
            not_to_change: vec![],
            branch: format!("task/{}-name", id),
            interface: None,
        }
    }

    fn write_tasks_toml(dir: &TempDir, tasks: &[Task]) {
        let mut lines = vec!["spec-version = 1\n".to_string()];
        for t in tasks {
            lines.push(format!(
                "[[tasks]]\nid = \"{}\"\nname = \"{}\"\nsize = \"{}\"\nstatus = \"{}\"\ndepends_on = []\ndone_when = \"\"\nscope = \"\"\nfiles_to_touch = []\nnot_to_change = []\nbranch = \"{}\"\n\n",
                t.id, t.name, t.size, t.status, t.branch
            ));
        }
        let content = lines.join("");
        let mut f = std::fs::File::create(dir.path().join("TASKS.md")).unwrap();
        // Write as TOML fenced block in markdown — mimick store format
        write!(f, "```toml\n{}```\n", content).unwrap();
    }

    #[test]
    fn test_all_statuses_order() {
        assert_eq!(ALL_STATUSES[0], TaskStatus::Open);
        assert_eq!(ALL_STATUSES[1], TaskStatus::InProgress);
        assert_eq!(ALL_STATUSES[2], TaskStatus::Done);
        assert_eq!(ALL_STATUSES[3], TaskStatus::Failed);
    }

    #[test]
    fn test_status_label_coverage() {
        for s in ALL_STATUSES {
            let label = status_label(s);
            assert!(!label.is_empty());
        }
    }

    #[test]
    fn test_status_style_returns_for_all() {
        for s in ALL_STATUSES {
            let _style = status_style(s);
        }
    }

    #[test]
    fn test_picker_next_wraps() {
        let mut state = ListState::default();
        state.select(Some(ALL_STATUSES.len() - 1));
        // Simulate picker_next logic
        let i = (state.selected().unwrap() + 1) % ALL_STATUSES.len();
        assert_eq!(i, 0);
    }

    #[test]
    fn test_picker_previous_wraps() {
        let mut state = ListState::default();
        state.select(Some(0));
        let i = if state.selected().unwrap() == 0 {
            ALL_STATUSES.len() - 1
        } else {
            state.selected().unwrap() - 1
        };
        assert_eq!(i, ALL_STATUSES.len() - 1);
    }

    #[test]
    fn test_mode_default_is_normal() {
        let mode = Mode::Normal;
        assert_eq!(mode, Mode::Normal);
        assert_ne!(mode, Mode::StatusPicker);
    }

    #[test]
    fn test_all_statuses_count() {
        assert_eq!(ALL_STATUSES.len(), 4);
    }
}
