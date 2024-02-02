use crossterm::event::KeyCode::Char;
use crossterm::event::{EventStream, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::ScrollUp;
use crossterm::ExecutableCommand;
use futures_channel::mpsc::Receiver;
use futures_channel::oneshot::Sender;
use futures_util::stream::select_all;
use futures_util::{future, StreamExt};
use log::error;
use ratatui::backend::{Backend, ClearType, CrosstermBackend};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, Paragraph, Widget};
use ratatui::{Frame, Terminal};
use std::collections::VecDeque;
use std::io;
use std::time::Duration;
use tokio::time;
use tokio_stream::wrappers::IntervalStream;
use uuid::Uuid;

use crate::{
    action::Action, action::ActionMessage, action::State, action::StatefulAction, error::Error,
};

const SPINNER_SYMBOLS: [&str; 6] = ["⠇", "⠋", "⠙", "⠸", "⠴", "⠦"];
const TICK_MS: u64 = 100;

#[derive(Debug)]
/// Tui App state
struct App {
    /// A queue of rows to show on the ui
    rows: VecDeque<Row>,
    /// A queue of actions to be confirmed. Each action is a tuple containing the id of the next action, the next action, and optionally a sender to notify the app that the next action was accepted.
    pending_actions: VecDeque<(Uuid, StatefulAction, Option<Sender<bool>>)>,
    /// The current tui area. Will grow when adding items and stop at the terminal border
    area: Rect,
    /// The space to leave for UI after showing actions on the bottom of the area
    bottom_margin: u16,
    /// Quit the app if true
    should_quit: bool,
    /// Index of the current symbol of the spinner
    spinner_index: usize,
}

#[derive(Debug)]
enum Row {
    ActionRow(Uuid, StatefulAction),
    StringRow(String),
}

#[derive(Debug)]
enum Event {
    Tick,
    Key(KeyEvent),
    Resize(u16, u16),
    Message(ActionMessage),
}

impl App {
    fn new(area: Rect) -> App {
        App {
            rows: VecDeque::new(),
            pending_actions: VecDeque::new(),
            area,
            bottom_margin: 0,
            should_quit: false,
            spinner_index: 0,
        }
    }
}

impl<'a> Into<Line<'a>> for &'a StatefulAction {
    fn into(self) -> Line<'a> {
        let state_span = match self.state {
            State::Running => "✔".blue(),
            State::Finished => "✔".green(),
            State::Pending => Span::raw("?"),
            State::Canceled => "✖".red(),
        };
        let first_separator_span = " │ ".dark_gray();
        let type_span = match &self.action {
            Action::Command { command: _ } => "> ".cyan().on_black(),
            Action::Read { path: _ } => "¶ ".cyan().on_black(),
            Action::Write {
                path: _,
                content: _,
            } => "🖉 ".cyan().on_black(),
        };
        let second_separator_span = " ".white();
        let mut content_span = match &self.action {
            Action::Command { command } => Span::raw(command),
            Action::Read { path } => Span::raw(path.to_string_lossy()),
            Action::Write { path, content } => {
                Span::raw(format!("{} {}", path.to_string_lossy(), content))
            }
        };
        if self.state != State::Pending {
            content_span = content_span.white();
        }
        Line::from(vec![
            state_span,
            first_separator_span,
            type_span,
            second_separator_span,
            content_span,
        ])
    }
}

impl<'a> Into<Line<'a>> for &'a Row {
    fn into(self) -> Line<'a> {
        match self {
            Row::ActionRow(_, action) => action.into(),
            Row::StringRow(s) => Line::from(s.as_str()),
        }
    }
}

pub async fn run(rx: Receiver<ActionMessage>) -> Result<(), Error> {
    // Setup terminal and app
    crossterm::terminal::enable_raw_mode()?;
    let stdout = io::stdout();
    let backend = CrosstermBackend::new(stdout);

    let mut terminal = Terminal::new(backend)?;
    let cursor_pos = terminal.get_cursor()?;
    let initial_area = Rect::new(0, cursor_pos.1, terminal.size()?.width, 0);
    let mut app = App::new(initial_area);

    terminal.hide_cursor()?;

    // Crossterm events
    let reader = EventStream::new()
        .filter_map(|result| async move {
            match result {
                Ok(event) => process_event(event),
                Err(e) => {
                    error!("Error in stream: {}", e);
                    None
                }
            }
        })
        .boxed();

    // App ticks (redraw events)
    let ticks = IntervalStream::new(time::interval(Duration::from_millis(TICK_MS)))
        .map(|_| Event::Tick)
        .boxed();

    // App messages (from session)
    let messages = rx
        .filter_map(|message| async { process_message(message) })
        .boxed();

    // Process the events and update the ui
    let _: () = select_all(vec![reader, ticks, messages])
        .scan(
            (&mut app, &mut terminal),
            |(ref mut app, ref mut terminal), event| {
                let result = update(app, terminal, event);

                // Draw screen if update was successful
                if result.is_ok() {
                    terminal
                        .draw(|f| ui(&app, f))
                        .map_err(|e| error!("Error: {}", e))
                        .ok();
                    terminal
                        .hide_cursor()
                        .map_err(|e| error!("Error: {}", e))
                        .ok();
                }

                // Stop early if the app should exit
                future::ready((!app.should_quit).then_some(()))
            },
        )
        .collect()
        .await;

    // Restore terminal state
    terminal.set_cursor(0, app.area.bottom())?;
    if app.area.height > 0 && app.area.bottom() == terminal.size()?.bottom() {
        terminal
            .backend_mut()
            .execute(crossterm::terminal::ScrollUp(1))?;
    }
    terminal.show_cursor()?;
    crossterm::terminal::disable_raw_mode()?;

    Ok(())
}

fn process_event(event: crossterm::event::Event) -> Option<Event> {
    match event {
        crossterm::event::Event::Key(key) if key.kind == KeyEventKind::Press => {
            Some(Event::Key(key))
        }
        crossterm::event::Event::Resize(cols, rows) => Some(Event::Resize(cols, rows)),
        _ => None,
    }
}

fn process_message(message: ActionMessage) -> Option<Event> {
    Some(Event::Message(message))
}

/// This function scrolls up the terminal and the internal buffer
fn scroll(
    terminal: &mut Terminal<CrosstermBackend<impl std::io::Write>>,
    height: u16,
) -> io::Result<()> {
    terminal.backend_mut().execute(ScrollUp(height))?;
    terminal.swap_buffers();
    Ok(())
}
/// This function insert lines in the history of the terminal (outside of the view area)
/// We're using the top of the terminal to render the lines, then scroll them out of view
fn insert_before<F>(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<impl std::io::Write>>,
    height: u16,
    draw_fn: F,
) -> io::Result<()>
where
    F: FnOnce(&mut Buffer),
{
    // Draw contents into buffer
    let area = Rect {
        x: app.area.left(),
        y: app.area.top(),
        width: app.area.width,
        height,
    };
    let mut buffer = Buffer::empty(area);
    draw_fn(&mut buffer);
    // Draw the buffer in the terminal
    terminal
        .backend_mut()
        .draw(buffer.content.iter().enumerate().map(|(i, c)| {
            let (x, y) = buffer.pos_of(i);
            (x, y, c)
        }))?;
    terminal.backend_mut().flush()?;
    // Scroll up
    scroll(terminal, height)?;
    Ok(())
}

// Update the internal state of the app. This function panics if the terminal is too small.
fn update(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<impl std::io::Write>>,
    event: Event,
) -> Result<(), Error> {
    match event {
        Event::Key(key) => match key.code {
            Char('q') => {
                app.should_quit = true;
            }
            Char('c') => {
                if key.modifiers == KeyModifiers::CONTROL {
                    app.should_quit = true;
                }
            }
            Char('y') => {
                if let Some((id, mut action, tx)) = app.pending_actions.pop_front() {
                    action.state = State::Running;
                    app.rows.push_back(Row::ActionRow(id, action));

                    if let Some(tx) = tx {
                        tx.send(true)
                            .map_err(|e| error!("Error sending confirmation: {}", e))
                            .ok();
                    }
                }
            }
            Char('n') => {
                if let Some((id, mut action, tx)) = app.pending_actions.pop_front() {
                    action.state = State::Canceled;
                    app.rows.push_back(Row::ActionRow(id, action));
                    if let Some(tx) = tx {
                        tx.send(false)
                            .map_err(|e| error!("Error sending confirmation: {}", e))
                            .ok();
                    }
                }
            }
            _ => (),
        },
        Event::Tick => {
            app.spinner_index = (app.spinner_index + 1) % SPINNER_SYMBOLS.len();
        }
        Event::Resize(width, height) => {
            terminal.resize(Rect::new(0, 0, width, height))?;
            app.area = Rect::new(0, 0, width, app.area.height.min(height));
        }
        Event::Message(message) => match message {
            ActionMessage::ConfirmAction((id, action, tx)) => {
                app.pending_actions.push_back((
                    id,
                    StatefulAction {
                        action,
                        state: State::Pending,
                    },
                    Some(tx),
                ));
            }
            ActionMessage::AddAction((id, action)) => {
                app.rows.push_back(Row::ActionRow(
                    id,
                    StatefulAction {
                        action,
                        state: State::Running,
                    },
                ));
            }
            ActionMessage::StopAction(id) => {
                if let Some(Row::ActionRow(_, action)) = app
                    .rows
                    .iter_mut()
                    .find(|row| matches!(row, Row::ActionRow(id_other, _) if id_other == &id))
                {
                    action.state = State::Finished
                }
            }
            ActionMessage::NewSession(session_id) => {
                app.rows
                    .push_back(Row::StringRow(format!("Session id: {}", session_id)));
            }
        },
    }

    app.bottom_margin = if app.pending_actions.len() > 0 { 2 } else { 0 };

    let terminal_size = terminal.size()?;

    if terminal_size.height < app.bottom_margin {
        return Err(Error::TerminalTooSmall);
    }

    // If the rows don't fit into the screen area we need to either grow the screen area
    // or scroll some rows out of the screen area.
    while app.rows.len() as i64 > (app.area.height as i64 - app.bottom_margin as i64) {
        // If there is still space in the terminal, grow the area towards the bottom
        if app.area.bottom() < terminal_size.bottom() {
            app.area.height += 1;
            continue;
        }
        // There is no space left at the bottom, we need to scroll up
        // We need to first clear the bottom margin
        terminal.set_cursor(0, app.area.bottom().saturating_sub(app.bottom_margin))?;
        terminal
            .backend_mut()
            .clear_region(ClearType::AfterCursor)?;
        // Maybe we can use space above
        if app.area.y > 0 {
            app.area.y -= 1;
            app.area.height += 1;
            scroll(terminal, 1)?;
        }
        // If there is no space left, we need to push a row out of view
        else if let Some(row) = app.rows.pop_front() {
            // This requires redrawing the row before pushing it out of view
            insert_before(app, terminal, 1, |buf| {
                let line: Line = (&row).into();
                Paragraph::new(line).render(buf.area, buf);
            })?;
        }
    }
    Ok(())
}

fn ui(app: &App, f: &mut Frame) {
    let area = f.size().intersection(app.area);

    let height = app.rows.len() as u16;

    // Create the layout, a list and optionally a confirmation dialog
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![
            Constraint::Length(height),
            Constraint::Length(app.bottom_margin),
        ])
        .split(area);

    let mut list_items = vec![];

    // Render the lines and spinners
    for row in &app.rows {
        let mut line: Line = row.into();
        if let Row::ActionRow(_, stateful_action) = row {
            if !app.should_quit && stateful_action.state == State::Running {
                if let State::Running = stateful_action.state {
                    line.spans[0] = Span::raw(SPINNER_SYMBOLS[app.spinner_index]).blue();
                }
            }
        }
        list_items.push(ListItem::new(line));
    }
    let list = List::new(list_items);
    f.render_widget(list, layout[0]);

    // Render the confirmation dialog
    if let Some((_, next_action, _)) = app.pending_actions.front() {
        let confirmation = Paragraph::new(vec![
            next_action.into(),
            Line::from("Are you sure you want to do this? [y/n]"),
        ]);
        f.render_widget(confirmation, layout[1]);
    }
}
