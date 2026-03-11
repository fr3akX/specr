use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Wrap},
};
use std::io::stdout;
use std::path::Path;

use crate::store;
use crate::types::{TaskSize, TaskStatus};

struct App {
    tasks: Vec<crate::types::Task>,
    spec_version: u32,
    project_name: String,
    table_state: TableState,
    detail_visible: bool,
    should_quit: bool,
    dir: std::path::PathBuf,
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
        })
    }

    fn refresh(&mut self) -> Result<()> {
        let (tasks, spec_version) = store::read_tasks(&self.dir)?;
        self.tasks = tasks;
        self.spec_version = spec_version;
        // Keep selection in bounds
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
            match key.code {
                KeyCode::Char('q') => {
                    app.should_quit = true;
                    return Ok(());
                }
                KeyCode::Char('r') => {
                    app.refresh().ok();
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
            }
        }
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
            let status_style = match task.status {
                TaskStatus::Done => Style::default().fg(Color::Green),
                TaskStatus::InProgress => Style::default().fg(Color::Cyan),
                TaskStatus::Open => Style::default(),
                TaskStatus::Failed => Style::default().fg(Color::Red),
            };
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
                Cell::from(status_str).style(status_style),
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

    // Footer
    let footer_text = " [q] quit  [r] refresh  [\u{2191}\u{2193}] navigate  [enter] toggle detail  [esc] close detail ";
    let footer = Paragraph::new(footer_text)
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(footer, chunks[3]);
}
