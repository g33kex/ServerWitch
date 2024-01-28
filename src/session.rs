//! Handles a session on the ServerWitch server
use futures_channel::mpsc::Sender;
use futures_util::{stream::select, StreamExt, TryStreamExt};
use log::{error, info};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::{net::TcpStream, time};
use tokio_stream::wrappers::IntervalStream;
use tokio_tungstenite::{
    connect_async, tungstenite::protocol::Message, MaybeTlsStream, WebSocketStream,
};
use uuid::Uuid;

use crate::action::{Action, ActionMessage, ActionResponse};
use crate::error::Error;

const KEEPALIVE_MESSAGE: &[u8] = b"keepalive";
const KEEPALIVE_INTERVAL: u64 = 20;
const MAX_CONCURRENCY: usize = 100;

/// Represents a session with an associated session ID and WebSocketStream
#[derive(Debug)]
pub struct Session {
    pub session_id: String,
    ws_stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

/// Represents a handshake message
#[derive(Serialize, Deserialize)]
struct Handshake {
    session_id: String,
}

/// Represents a request
#[derive(Serialize, Deserialize, Debug)]
struct Request {
    data: Action,
    request_id: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct Response {
    data: ActionResponse,
    error: bool,
    request_id: String,
}
impl Session {
    /// Connect to the ServerWitch server and obtain a new session
    pub async fn new(url: &str) -> Result<Session, Error> {
        let (mut ws_stream, _) = connect_async(url).await?;

        let Message::Text(text) = ws_stream.next().await.ok_or(Error::NoSessionId)?? else {
            return Err(Error::NoSessionId);
        };

        let handshake: Handshake = serde_json::from_str(&text)?;

        let session_id = handshake.session_id;
        Ok(Session {
            session_id,
            ws_stream,
        })
    }

    /// Process messages from the server. This method consumes the session.
    pub async fn process_messages(self, no_confirm: bool, tx: Sender<ActionMessage>) {
        let (sink, stream) = self.ws_stream.split();

        let requests = stream
            // Print errors and messages
            .inspect_err(|e| error!("Error in stream: {}", e))
            .inspect_ok(|r| info!("Received message: {}", r))
            // Remove errors from stream
            .filter_map(|result| async { result.ok() })
            // Handle messages concurrently
            .map(|message| handle_message(message, no_confirm, tx.clone()))
            .buffer_unordered(MAX_CONCURRENCY)
            // Print errors in message handling
            .inspect_err(|e| error!("Error processing message: {}", e))
            // Remove errors and options from stream and return a result
            .filter_map(|result| async { result.ok().flatten().map(Ok) });

        // Send recurrent pings to keep the connection alive
        let keepalives =
            IntervalStream::new(time::interval(Duration::from_secs(KEEPALIVE_INTERVAL)))
                .map(|_| Ok(Message::Ping(KEEPALIVE_MESSAGE.to_vec())));

        select(requests, keepalives)
            .inspect_ok(|r| info!("Sending message: {}", r))
            .forward(sink)
            .await
            .unwrap_or_else(|e| error!("Error in sink: {}", e));
    }
}

/// Notify the TUI of an action and optionally get user confirmation
/// Returns the UUID of the action if it was confirmed
async fn get_confirmation(
    action: &Action,
    no_confirm: bool,
    tx: &mut Sender<ActionMessage>,
) -> Option<Uuid> {
    let uuid = Uuid::new_v4();
    if no_confirm {
        let message = ActionMessage::AddAction((uuid, action.clone()));
        tx.try_send(message)
            .map_err(|e| error!("Error sending message: {}", e))
            .ok()
            .and(Some(uuid))
    } else {
        let (tx_r, rx_r) = futures_channel::oneshot::channel();
        let message = ActionMessage::ConfirmAction((uuid, action.clone(), tx_r));
        if tx
            .try_send(message)
            .map_err(|e| error!("Error sending message: {}", e))
            .is_err()
        {
            return None;
        }
        rx_r.await
            .map_err(|e| error!("Error getting confirmation: {}", e))
            .ok()
            .and_then(|b| if b { Some(uuid) } else { None })
    }
}

/// Handles messages received on the websocket and respond
async fn handle_message(
    message: Message,
    no_confirm: bool,
    tx: Sender<ActionMessage>,
) -> Result<Option<Message>, Error> {
    match message {
        Message::Text(payload) => {
            let request: Request = serde_json::from_str(&payload)?;
            let response = serde_json::to_string(&handle_request(request, no_confirm, tx).await)?;
            Ok(Some(Message::Text(response)))
        }
        Message::Ping(payload) => Ok(Some(Message::Pong(payload))),
        Message::Pong(_) => Ok(None),
        Message::Close(_) => Ok(None),
        _ => Err(Error::UnsupportedMessage),
    }
}

/// Handle a request and respond
async fn handle_request(
    request: Request,
    no_confirm: bool,
    mut tx: Sender<ActionMessage>,
) -> Response {
    let action = request.data;

    let uuid = get_confirmation(&action, no_confirm, &mut tx).await;
    if let Some(uuid) = uuid {
        let result = action.execute().await;

        // Notify the TUI that the action is done
        tx.try_send(ActionMessage::StopAction(uuid))
            .map_err(|e| error!("Failed to send done event: {}", e))
            .ok();

        Response {
            request_id: request.request_id,
            error: result.is_err(),
            data: result.unwrap_or_else(|e| ActionResponse::Error(e.to_string())),
        }
    } else {
        Response {
            request_id: request.request_id,
            error: true,
            data: ActionResponse::Error(String::from("The user refused to run the command")),
        }
    }
}
