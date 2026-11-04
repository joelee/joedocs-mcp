//! Helpers shared by the MCP test crates: an rmcp client with a bearer key,
//! tool calls that expect success or failure, and raw HTTP requests.
//!
//! Helpers return `Result`, because only `#[test]` functions may unwrap.

use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, Implementation,
};
use rmcp::service::RunningService;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::{RoleClient, ServiceExt as _};
use serde_json::Value;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

/// A connected MCP client.
pub type Client = RunningService<RoleClient, ClientConfig>;

/// Result type for helpers.
pub type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// A minimal `initialize` request body.
pub const INIT: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"raw","version":"1"}}}"#;

/// Connects to the MCP endpoint `url` with `token`, as client `test-client`.
pub async fn connect(url: &str, token: &str) -> TestResult<Client> {
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(url.to_owned()).auth_header(token),
    );
    Ok(ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("test-client", "1.0"),
    )
    .serve(transport)
    .await?)
}

/// Calls a tool with a JSON object of arguments.
pub async fn call(client: &Client, tool: &str, args: Value) -> TestResult<CallToolResult> {
    let args = args.as_object().cloned().unwrap_or_default();
    Ok(client
        .call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(args))
        .await?)
}

/// The structured result of a call that must succeed.
pub async fn ok(client: &Client, tool: &str, args: Value) -> TestResult<Value> {
    let result = call(client, tool, args).await?;
    assert_ne!(result.is_error, Some(true), "{tool}: {result:?}");
    Ok(result.structured_content.ok_or("no structured content")?)
}

/// The `error` object of a call that must fail.
pub async fn error(client: &Client, tool: &str, args: Value) -> TestResult<Value> {
    let result = call(client, tool, args).await?;
    assert_eq!(result.is_error, Some(true), "{tool}: {result:?}");
    let content = result.structured_content.ok_or("no structured content")?;
    Ok(content["error"].clone())
}

/// Sends a raw HTTP/1.1 request to `authority` (`host:port`) and returns
/// the status code.
pub async fn status(authority: &str, request: &str) -> TestResult<u16> {
    let mut stream = TcpStream::connect(authority).await?;
    stream.write_all(request.as_bytes()).await?;
    let mut response = Vec::new();
    let mut buf = [0u8; 1024];
    while !response.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        response.extend_from_slice(&buf[..n]);
    }
    let text = String::from_utf8_lossy(&response);
    Ok(text.split(' ').nth(1).ok_or("no status line")?.parse()?)
}

/// A raw `POST /mcp` request with `Host: host`, extra header lines and a
/// JSON body.
pub fn post(host: &str, extra: &str, body: &str) -> String {
    format!(
        "POST /mcp HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\n{extra}Content-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )
}
