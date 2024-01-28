//! Perform actions on the system such as running command, reading and writing files
use crate::error::Error;
use futures_channel::oneshot;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::fs;
use tokio::process::Command;
use uuid::Uuid;

const SHELL: &str = "/bin/bash";
const SHELL_ARGS: [&str; 1] = ["-c"];

/// Represents an action
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum Action {
    Read { path: PathBuf },
    Command { command: String },
    Write { path: PathBuf, content: String },
}

/// Represents the response to an action
#[derive(Serialize, Deserialize, Debug)]
#[serde(untagged, rename_all = "lowercase")]
pub enum ActionResponse {
    Read {
        content: String,
    },
    Command {
        return_code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    Write {
        size: usize,
    },
    Error(String),
}

#[derive(Debug, PartialEq, Clone)]
/// The state of an action
pub enum State {
    Running,
    Finished,
    Pending,
    Canceled,
}

#[derive(Debug, Clone)]
/// An action with a state
pub struct StatefulAction {
    pub action: Action,
    pub state: State,
}

/// Messages sent by the application about actions (to UI, logs, ...)
#[derive(Debug)]
pub enum ActionMessage {
    /// Confirm action before adding. Tuple of id, stateful action, and a channel to send back confirmation
    ConfirmAction((Uuid, Action, oneshot::Sender<bool>)),
    /// Add action without confirmation. Tuple of id, action
    AddAction((Uuid, Action)),
    /// Indicate that an action has terminated
    StopAction(Uuid),
    /// Indicate that a session was started
    NewSession(String),
}
/// Run a command and return the output
async fn run_command(command: &str) -> Result<ActionResponse, Error> {
    let output = Command::new(SHELL)
        .stdin(Stdio::null())
        .args(SHELL_ARGS)
        .arg(command)
        .output()
        .await?;
    Ok(ActionResponse::Command {
        return_code: output.status.code(),
        stdout: String::from_utf8(output.stdout)?,
        stderr: String::from_utf8(output.stderr)?,
    })
}

impl Action {
    /// Execute the action and return a response
    pub async fn execute(&self) -> Result<ActionResponse, Error> {
        match self {
            Action::Command { command } => run_command(&command).await,
            Action::Read { path } => read_file(&path).await,
            Action::Write { content, path } => write_file(&path, &content).await,
        }
    }
}

/// Read a file and return the content
async fn read_file(path: &Path) -> Result<ActionResponse, Error> {
    let content = fs::read_to_string(path).await?;
    Ok(ActionResponse::Read { content })
}

/// Write in a file and return the number of bytes written
async fn write_file(path: &Path, content: &str) -> Result<ActionResponse, Error> {
    fs::write(path, content).await?;
    Ok(ActionResponse::Write {
        size: content.len(),
    })
}
