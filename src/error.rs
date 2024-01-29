///! Errors used in this crate
use std::string::FromUtf8Error;
use thiserror::Error;
use tokio_tungstenite::tungstenite::Message;

use crate::action::ActionMessage;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Cannot connect to server")]
    CannotConnect(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("Failed to parse the session ID")]
    SessionIdParseFailure(#[from] serde_json::Error),
    #[error("Failed to obtain a session ID")]
    NoSessionId,
    #[error("The message type sent by the server is unsupported")]
    UnsupportedMessage,
    #[error("Could not send the message into the channel")]
    SendError(#[from] futures_channel::mpsc::TrySendError<Message>),
    #[error("Error executing command: {0}")]
    CommandError(#[from] std::io::Error),
    #[error("Command output contains invalid characters")]
    CommandOutputError(#[from] FromUtf8Error),
    #[error("Could not send the action to the tui")]
    ActionSendError(#[from] futures_channel::mpsc::TrySendError<ActionMessage>),
}
