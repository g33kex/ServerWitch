use clap::Parser;
use futures_channel::mpsc;
use futures_util::FutureExt;
use futures_util::{pin_mut, select};
use log::info;
use log::LevelFilter;
use std::path::PathBuf;

use serverwitch::action::ActionMessage;
use serverwitch::session::Session;
use serverwitch::tui;

const SERVER_URL: &str = "wss:///serverwitch.dev";
const LOG_FILE: &str = "serverwitch.log";
const CHANNEL_SIZE: usize = 100;

/// The Cli arguments
#[derive(Parser, Debug)]
#[clap(author = "g33kex, g33kex@pm.me", version = env!("CARGO_PKG_VERSION"), about = "Let an AI remotely control your computer", long_about = None)]
struct Cli {
    /// The URL of the ServerWitch relay server
    #[arg(short, long, default_value_t = url::Url::parse(SERVER_URL).expect("Failed to parse default server URL"))]
    url: url::Url,
    /// Path to write logs
    #[arg(short, long, default_value = LOG_FILE)]
    output_file: PathBuf,
    /// DANGEROUS: Execute all commands without confirmation
    #[arg(long = "yes")]
    noconfirm: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Cli::parse();

    let _ = simple_logging::log_to_file(args.output_file, LevelFilter::Info);

    let url = args.url.join("/session")?;
    let session = Session::new(url.as_str()).await?;
    info!("Session id: {}", session.session_id);

    let (mut tx, rx) = mpsc::channel(CHANNEL_SIZE);

    tx.try_send(ActionMessage::NewSession(session.session_id.clone()))?;

    let tui_task = tui::run(rx).fuse();
    let session_task = session.process_messages(args.noconfirm, tx).fuse();

    pin_mut!(tui_task, session_task);

    select!(_ = tui_task => info!("Application closed"), _ = session_task => info!("Session closed"));

    Ok(())
}
