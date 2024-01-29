use crossterm::event::KeyCode::Char;
use crossterm::event::{EventStream, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::ExecutableCommand;
use futures_channel::mpsc::Receiver;
use futures_channel::oneshot::Sender;
use futures_util::stream::select_all;
use futures_util::{future, StreamExt};
use log::error;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, Paragraph, Widget};
use ratatui::{Frame, Terminal, Viewport};
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
    /// A queue of confirmed actions
    actions: VecDeque<(Uuid, StatefulAction)>,
    /// A queue of actions to be confirmed. Each action is a tuple containing the id of the next action, the next action, and optionally a sender to notify the app that the next action was accepted.
    pending_actions: VecDeque<(Uuid, StatefulAction, Option<Sender<bool>>)>,
    view_height: u16,
    should_quit: bool,
    spinner_index: usize,
}

#[derive(Debug)]
enum Event {
    Tick,
    Key(KeyEvent),
    Resize(u16, u16),
    Message(ActionMessage),
}

impl App {
    fn new(view_height: u16) -> App {
        App {
            actions: VecDeque::new(),
            pending_actions: VecDeque::new(),
            view_height,
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

pub async fn run(rx: Receiver<ActionMessage>) -> Result<(), Error> {
    // Setup terminal and app
    crossterm::terminal::enable_raw_mode()?;
    let stdout = io::stdout();
    let backend = CrosstermBackend::new(stdout);
    let view_height = backend.size()?.height / 2;

    let mut app = App::new(view_height);

    let mut terminal = Terminal::with_options(
        backend,
        ratatui::TerminalOptions {
            viewport: Viewport::Inline(view_height),
        },
    )?;
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
                update(app, terminal, event);
                terminal
                    .draw(|f| ui(&app, f))
                    .map_err(|e| error!("Error: {}", e))
                    .ok();
                terminal
                    .hide_cursor()
                    .map_err(|e| error!("Error: {}", e))
                    .ok();

                // Stop early if the app should exit
                future::ready((!app.should_quit).then_some(()))
            },
        )
        .collect()
        .await;

    // Restore terminal state
    terminal
        .backend_mut()
        .execute(crossterm::terminal::ScrollUp(1))?
        .execute(crossterm::cursor::MoveToColumn(0))?;
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

// Update the internal state of the app. This should never fail.
fn update(app: &mut App, terminal: &mut Terminal<impl Backend>, event: Event) {
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
                    app.actions.push_back((id, action));

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
                    app.actions.push_back((id, action));
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
        Event::Resize(_, _) => {
            terminal
                .autoresize()
                .map_err(|e| error!("Error resizing terminal: {}", e))
                .ok();
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
                app.actions.push_back((
                    id,
                    StatefulAction {
                        action,
                        state: State::Running,
                    },
                ));
            }
            ActionMessage::StopAction(id) => {
                if let Some((_, action)) = app
                    .actions
                    .iter_mut()
                    .find(|(id_other, _)| id_other.eq(&id))
                {
                    action.state = State::Finished
                }
            }
            ActionMessage::NewSession(session_id) => {
                terminal
                    .insert_before(1, |buf| {
                        Paragraph::new(Line::from(format!("Session id: {}", session_id)))
                            .render(buf.area, buf);
                    })
                    .map_err(|e| error!("Error inserting line in terminal: {}", e))
                    .ok();
            }
        },
    }

    while app.actions.len() > app.view_height as usize - 2 {
        if let Some((_, stateful_action)) = app.actions.pop_front() {
            terminal
                .insert_before(1, |buf| {
                    let line: Line = (&stateful_action).into();
                    Paragraph::new(line).render(buf.area, buf);
                })
                .map_err(|e| error!("Error inserting line in terminal: {}", e))
                .ok();
        }
    }
}

fn ui(app: &App, f: &mut Frame) {
    let area = f.size();
    let height = app.actions.len() as u16;
    let has_next_action = app.pending_actions.len() > 0;

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![
            Constraint::Length(height),
            Constraint::Length(if has_next_action { 2 } else { 0 }),
        ])
        .split(area);

    let mut list_items = vec![];
    for (_, stateful_action) in &app.actions {
        let mut line: Line = stateful_action.into();
        if !app.should_quit && stateful_action.state == State::Running {
            if let State::Running = stateful_action.state {
                line.spans[0] = Span::raw(SPINNER_SYMBOLS[app.spinner_index]).blue();
            }
        }
        list_items.push(ListItem::new(line));
    }
    let list = List::new(list_items);
    f.render_widget(list, layout[0]);

    if let Some((_, next_action, _)) = app.pending_actions.front() {
        let confirmation = Paragraph::new(vec![
            next_action.into(),
            Line::from("Are you sure you want to do this? [y/n]"),
        ]);
        f.render_widget(confirmation, layout[1]);
    }

    f.set_cursor(
        0,
        area.top() + (height as u16).saturating_sub(if has_next_action { 0 } else { 1 }),
    );
}
