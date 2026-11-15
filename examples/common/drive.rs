//! Shared by the drive examples (`board_drive.rs`, `board_drive_showcase.rs`),
//! which include it with `#[path = "common/drive.rs"] mod drive;`. Cargo
//! builds every `examples/*.rs` file as an example of its own, so this one
//! sits in a folder without a `main.rs`, which Cargo does not pick up.
//!
//! An MCP client with one API key, the calls the drives make, and the pause
//! between their steps: a time, or with `DRIVE_PAUSE_MS=0` a keystroke.

use std::error::Error;
use std::io::{Read as _, Write as _};
use std::time::Duration;

use rmcp::model::{CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation};
use rmcp::service::RunningService;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::{RoleClient, ServiceExt as _};
use serde_json::{Value, json};

pub type Client = RunningService<RoleClient, ClientConfig>;
pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

/// Connects to the MCP endpoint `url` with the API key `token`, reporting
/// itself as the MCP client `name` (the `client` of each revision).
pub async fn connect(url: &str, token: &str, name: &str) -> Result<Client> {
    // reqwest is built with rustls but no crypto provider, as in the tests.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(url.to_owned()).auth_header(token.to_owned()),
    );
    let client = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new(name.to_owned(), "1.0"),
    )
    .serve(transport)
    .await?;
    Ok(client)
}

/// Calls `tool`, failing on a tool error, and returns its structured result.
pub async fn call(client: &Client, tool: &str, args: Value) -> Result<Value> {
    let args = args.as_object().cloned().unwrap_or_default();
    let result = client
        .call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(args))
        .await?;
    let content = result.structured_content.unwrap_or(Value::Null);
    if result.is_error == Some(true) {
        return Err(format!("{tool} failed: {content}").into());
    }
    Ok(content)
}

/// Sets front matter keys on `id`, and removes `remove`.
pub async fn front(client: &Client, id: &str, set: Value, remove: &[&str]) -> Result<()> {
    let mut args = json!({"document_id": id, "set": set});
    if !remove.is_empty() {
        args["remove"] = json!(remove);
    }
    call(client, "change_document_front_matter", args).await?;
    Ok(())
}

/// Creates a document and returns its ID.
pub async fn create(client: &Client, args: Value) -> Result<String> {
    let made = call(client, "new_document", args).await?;
    Ok(made["document_id"]
        .as_str()
        .ok_or("new_document returned no document_id")?
        .to_owned())
}

/// The pause between steps, from `DRIVE_PAUSE_MS` (default 3000): that many
/// milliseconds, or with `0` one keystroke on standard input. The script
/// that runs the drive puts the terminal in non-canonical mode, so one key
/// is one byte, with no Enter.
#[derive(Debug, Clone, Copy)]
pub enum Pause {
    Timed(Duration),
    Key,
}

impl Pause {
    pub fn from_env() -> Self {
        match std::env::var("DRIVE_PAUSE_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(3000)
        {
            0 => Self::Key,
            ms => Self::Timed(Duration::from_millis(ms)),
        }
    }

    /// One whole pause before the step `next`.
    pub async fn wait(&self, next: &str) -> Result<()> {
        match self {
            Self::Timed(d) => {
                tokio::time::sleep(*d).await;
                Ok(())
            }
            Self::Key => key(next).await,
        }
    }

    /// Half a pause (a keystroke all the same) before the step `next`.
    pub async fn half(&self, next: &str) -> Result<()> {
        match self {
            Self::Timed(d) => {
                tokio::time::sleep(*d / 2).await;
                Ok(())
            }
            Self::Key => key(next).await,
        }
    }
}

/// Waits for one byte on standard input, after a prompt naming `next`.
async fn key(next: &str) -> Result<()> {
    writeln!(std::io::stderr(), "press a key: next {next}")?;
    let read = tokio::task::spawn_blocking(|| {
        let mut byte = [0_u8; 1];
        std::io::stdin().read(&mut byte)
    })
    .await??;
    if read == 0 {
        return Err("standard input ended while waiting for a key".into());
    }
    Ok(())
}
