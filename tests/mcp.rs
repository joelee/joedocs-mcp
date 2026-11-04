//! End-to-end MCP tests: an in-process server on a loopback port, driven by
//! rmcp's own Streamable HTTP client, plus raw HTTP checks of the
//! authentication, `Host` and `Origin` rules, and one run of the real
//! `joedocs serve` binary.

use std::path::Path;
use std::sync::Arc;

use joedocs::Config;
use joedocs::auth::CountingRandom;
use joedocs::clock::FixedClock;
use joedocs::history::GitCli;
use joedocs::mcp::{ServerOptions, Shared, serve};
use joedocs::store::{Actor, NewDocument, Paths, Settings, Store};
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, Implementation,
};
use rmcp::service::RunningService;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::{RoleClient, ServiceExt as _};
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

type Client = RunningService<RoleClient, ClientConfig>;

/// Helpers return `Result`; only `#[test]` functions may unwrap.
type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct Server {
    _dir: TempDir,
    root: std::path::PathBuf,
    addr: std::net::SocketAddr,
    writer: String,
    reader: String,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

fn store(root: &Path) -> Result<Store, joedocs::store::Error> {
    let paths = Paths {
        home: root.join("vault"),
        state_dir: root.join("state"),
    };
    let git = GitCli::new(paths.git_dir(), &paths.home);
    Store::init(&paths, &git)?;
    Store::open(
        paths,
        Settings::from_config(&Config::default()),
        Box::new(git),
        Arc::new(FixedClock::at(1_793_000_000)),
    )
}

/// A server with projects `joedocs` (JD) and `secret` (SC, holding SC-1),
/// a `laptop` key with `joedocs:write` and a `viewer` key with
/// `joedocs:read`.
async fn start() -> TestResult<Server> {
    let dir = TempDir::new()?;
    let root = dir.path().to_owned();
    let mut store = store(&root)?;
    store.add_project("joedocs", "JD", Some("JoeDocs"))?;
    store.add_project("secret", "SC", None)?;
    store.create(
        &Actor::new("admin", "test"),
        NewDocument {
            project: "secret".into(),
            title: "Hidden".into(),
            content: "x".into(),
            ..NewDocument::default()
        },
    )?;
    let writer = store.create_key("laptop", &["joedocs:write".parse()?], &CountingRandom(0))?;
    let reader = store.create_key("viewer", &["joedocs:read".parse()?], &CountingRandom(100))?;

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let (stop, stopped) = oneshot::channel::<()>();
    let options = ServerOptions::from_config(&Config::default());
    let task = tokio::spawn(async move {
        serve(listener, Shared::new(store), &options, async {
            stopped.await.ok();
        })
        .await
    });
    Ok(Server {
        _dir: dir,
        root,
        addr,
        writer: writer.token.expose_secret().to_owned(),
        reader: reader.token.expose_secret().to_owned(),
        stop: Some(stop),
        task,
    })
}

impl Server {
    async fn client(&self, token: &str) -> TestResult<Client> {
        let transport = StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(format!("http://{}/mcp", self.addr))
                .auth_header(token),
        );
        Ok(ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("test-client", "1.0"),
        )
        .serve(transport)
        .await?)
    }

    async fn stop(mut self) -> TestResult<()> {
        if let Some(stop) = self.stop.take() {
            stop.send(()).ok();
        }
        Ok(self.task.await??)
    }
}

async fn call(client: &Client, tool: &str, args: Value) -> TestResult<CallToolResult> {
    let args = args.as_object().cloned().unwrap_or_default();
    Ok(client
        .call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(args))
        .await?)
}

/// The structured result of a call that must succeed.
async fn ok(client: &Client, tool: &str, args: Value) -> TestResult<Value> {
    let result = call(client, tool, args).await?;
    assert_ne!(result.is_error, Some(true), "{tool}: {result:?}");
    Ok(result.structured_content.ok_or("no structured content")?)
}

/// The `error` object of a call that must fail.
async fn error(client: &Client, tool: &str, args: Value) -> TestResult<Value> {
    let result = call(client, tool, args).await?;
    assert_eq!(result.is_error, Some(true), "{tool}: {result:?}");
    let content = result.structured_content.ok_or("no structured content")?;
    Ok(content["error"].clone())
}

/// Sends a raw HTTP/1.1 request and returns the status code.
async fn status(addr: std::net::SocketAddr, request: &str) -> TestResult<u16> {
    let mut stream = TcpStream::connect(addr).await?;
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

fn post(host: &str, extra: &str, body: &str) -> String {
    format!(
        "POST /mcp HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\n{extra}Content-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )
}

fn audit_log(root: &Path) -> TestResult<Vec<String>> {
    let conn = joedocs::db::open(&root.join("state/joedocs.db"))?;
    let mut stmt = conn.prepare(
        "SELECT tool, coalesce(document_id, '-'), outcome FROM audit_log ORDER BY rowid",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(format!(
            "{} {} {}",
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

const INIT: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"raw","version":"1"}}}"#;

#[tokio::test]
async fn tools_are_listed_with_schemas_and_hints() {
    let server = start().await.unwrap();
    let client = server.client(&server.writer).await.unwrap();

    let tools = client.list_all_tools().await.unwrap();
    let mut names: Vec<_> = tools.iter().map(|t| t.name.to_string()).collect();
    names.sort();
    assert_eq!(
        names,
        [
            "archive_document",
            "change_document_front_matter",
            "get_document",
            "list_documents",
            "list_projects",
            "new_document",
            "rename_document",
            "update_document",
        ]
    );
    for tool in &tools {
        assert!(
            tool.output_schema.is_some(),
            "{} has an output schema",
            tool.name
        );
        let read_only = tool.annotations.as_ref().and_then(|a| a.read_only_hint);
        let expected =
            ["get_document", "list_documents", "list_projects"].contains(&tool.name.as_ref());
        assert_eq!(read_only, Some(expected), "{}", tool.name);
    }

    client.cancel().await.unwrap();
    server.stop().await.unwrap();
}

/// The second half of the round trip: rename, front matter, list, old
/// versions and archive.
async fn rename_change_list_and_archive(client: &Client) -> TestResult<()> {
    let renamed = ok(
        client,
        "rename_document",
        json!({"document_id": "JD-1", "title": "MCP rollout"}),
    )
    .await?;
    assert_eq!(renamed["path"], "joedocs/plans/MCP rollout.md");
    assert_eq!(renamed["front_matter"]["aliases"], json!(["MCP plan"]));

    let changed = ok(
        client,
        "change_document_front_matter",
        json!({"document_id": "JD-1", "set": {"due": "2026-12-01"}, "remove": ["owner"],
               "expected_revision": 3}),
    )
    .await?;
    assert_eq!(changed["revision"], 4);
    assert!(changed["front_matter"].get("owner").is_none());

    let listed = ok(client, "list_documents", json!({"project": "joedocs"})).await?;
    assert_eq!(listed["total"], 1);
    assert_eq!(listed["items"][0]["title"], "MCP rollout");
    assert_eq!(listed["next_start"], Value::Null);

    let old = ok(
        client,
        "get_document",
        json!({"document_id": "JD-1", "version": 1}),
    )
    .await?;
    assert_eq!(old["revision"]["revision"], 1);

    let archived = ok(
        client,
        "archive_document",
        json!({"document_id": "JD-1", "reason": "done"}),
    )
    .await?;
    assert_eq!(archived["revision"], 5);
    let gone = error(client, "get_document", json!({"document_id": "JD-1"})).await?;
    assert_eq!(gone["code"], "archived");

    Ok(())
}

#[tokio::test]
async fn every_tool_round_trips() {
    let server = start().await.unwrap();
    let client = server.client(&server.writer).await.unwrap();

    let projects = ok(&client, "list_projects", json!({})).await.unwrap();
    assert_eq!(
        projects["projects"],
        json!([{"name": "joedocs", "key": "JD", "title": "JoeDocs", "permission": "write", "documents": 0}])
    );
    assert_eq!(projects["default_type"], "home");

    let created = ok(
        &client,
        "new_document",
        json!({
            "project": "joedocs",
            "type": "plans",
            "title": "MCP plan",
            "content": "# Plan\n",
            "front_matter": {"tags": ["mcp"], "owner": "joel"},
            "summary": "first draft"
        }),
    )
    .await
    .unwrap();
    assert_eq!(created["document_id"], "JD-1");
    assert_eq!(created["path"], "joedocs/plans/MCP plan.md");
    assert_eq!(created["front_matter"]["client"], "test-client");
    assert_eq!(created["front_matter"]["created_by"], "laptop");

    let got = ok(&client, "get_document", json!({"document_id": "JD-1"}))
        .await
        .unwrap();
    assert!(
        got["markdown"]
            .as_str()
            .unwrap()
            .ends_with("owner: joel\n---\n# Plan\n")
    );
    assert_eq!(got["document"]["type"], "plans");
    assert_eq!(got["revision"]["summary"], "Create JD-1: first draft");

    let stale = error(
        &client,
        "update_document",
        json!({"document_id": "JD-1", "content": "x", "expected_revision": 7}),
    )
    .await
    .unwrap();
    assert_eq!(stale["code"], "conflict");
    assert_eq!(stale["current_revision"], 1);

    let updated = ok(
        &client,
        "update_document",
        json!({"document_id": "JD-1", "content": "# Plan v2\n", "expected_revision": 1,
               "front_matter": {"status": "review"}}),
    )
    .await
    .unwrap();
    assert_eq!(updated["revision"], 2);
    assert_eq!(updated["front_matter"]["status"], "review");

    rename_change_list_and_archive(&client).await.unwrap();

    client.cancel().await.unwrap();
    let audit = audit_log(&server.root).unwrap();
    assert_eq!(
        audit,
        [
            "new_document JD-1 ok",
            "update_document JD-1 conflict",
            "update_document JD-1 ok",
            "rename_document JD-1 ok",
            "change_document_front_matter JD-1 ok",
            "archive_document JD-1 ok",
        ]
    );
    server.stop().await.unwrap();
}

#[tokio::test]
async fn grants_limit_what_a_key_can_see_and_change() {
    let server = start().await.unwrap();
    let writer = server.client(&server.writer).await.unwrap();
    let reader = server.client(&server.reader).await.unwrap();

    ok(
        &writer,
        "new_document",
        json!({"project": "joedocs", "title": "Shared", "content": "x"}),
    )
    .await
    .unwrap();

    let projects = ok(&reader, "list_projects", json!({})).await.unwrap();
    assert_eq!(projects["projects"][0]["permission"], "read");
    ok(&reader, "get_document", json!({"document_id": "JD-1"}))
        .await
        .unwrap()
        .get("markdown")
        .unwrap();
    let denied = error(
        &reader,
        "update_document",
        json!({"document_id": "JD-1", "content": "y", "expected_revision": 1}),
    )
    .await
    .unwrap();
    assert_eq!(denied["code"], "permission_denied");
    let denied = error(
        &reader,
        "new_document",
        json!({"project": "joedocs", "title": "No", "content": "x"}),
    )
    .await
    .unwrap();
    assert_eq!(denied["code"], "permission_denied");

    for (tool, args) in [
        ("get_document", json!({"document_id": "SC-1"})),
        (
            "archive_document",
            json!({"document_id": "SC-1", "reason": "x"}),
        ),
        ("list_documents", json!({"project": "secret"})),
        (
            "new_document",
            json!({"project": "secret", "title": "t", "content": "c"}),
        ),
        ("get_document", json!({"document_id": "ZZ-1"})),
    ] {
        let hidden = error(&writer, tool, args).await.unwrap();
        assert_eq!(hidden["code"], "not_found", "{tool}");
    }
    let invalid = error(&writer, "get_document", json!({"document_id": "nonsense"}))
        .await
        .unwrap();
    assert_eq!(invalid["code"], "invalid_argument");
    assert_eq!(invalid["field"], "document_id");

    let unknown = call(
        &writer,
        "get_document",
        json!({"document_id": "JD-1", "typo": 1}),
    )
    .await
    .unwrap();
    assert_eq!(
        unknown.is_error,
        Some(true),
        "unknown arguments are rejected"
    );

    writer.cancel().await.unwrap();
    reader.cancel().await.unwrap();
    let audit = audit_log(&server.root).unwrap();
    assert!(
        audit.contains(&"update_document JD-1 permission_denied".to_owned()),
        "{audit:?}"
    );
    assert!(
        audit.contains(&"archive_document SC-1 not_found".to_owned()),
        "{audit:?}"
    );
    server.stop().await.unwrap();
}

#[tokio::test]
async fn http_requests_need_a_valid_key_host_and_origin() {
    let server = start().await.unwrap();
    let addr = server.addr;
    let host = addr.to_string();
    let auth = format!("Authorization: Bearer {}\r\n", server.writer);

    let health = format!("GET /health HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    assert_eq!(status(addr, &health).await.unwrap(), 200);
    assert_eq!(status(addr, &post(&host, "", INIT)).await.unwrap(), 401);
    let wrong = format!(
        "Authorization: Bearer jd_{}_{}\r\n",
        "0".repeat(12),
        "1".repeat(64)
    );
    assert_eq!(status(addr, &post(&host, &wrong, INIT)).await.unwrap(), 401);
    assert_eq!(
        status(addr, &post(&host, "Authorization: Basic eDp5\r\n", INIT))
            .await
            .unwrap(),
        401
    );
    assert_eq!(status(addr, &post(&host, &auth, INIT)).await.unwrap(), 200);

    let evil_origin = format!("{auth}Origin: http://evil.example\r\n");
    assert_eq!(
        status(addr, &post(&host, &evil_origin, INIT))
            .await
            .unwrap(),
        403
    );
    assert_eq!(
        status(addr, &post("evil.example", &auth, INIT))
            .await
            .unwrap(),
        403
    );

    {
        let mut store = store(&server.root).unwrap();
        store.revoke("laptop").unwrap();
    }
    assert_eq!(status(addr, &post(&host, &auth, INIT)).await.unwrap(), 401);
    server.stop().await.unwrap();
}

#[test]
fn the_serve_command_runs_until_sigterm() {
    use std::io::{BufRead as _, BufReader};
    use std::process::{Command, Stdio};

    let dir = TempDir::new().unwrap();
    let env = |cmd: &mut Command| {
        cmd.env_clear()
            .env("HOME", dir.path())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("JOEDOCS__STORAGE__HOME", dir.path().join("vault"))
            .env("JOEDOCS__STORAGE__STATE_DIR", dir.path().join("state"))
            .env("JOEDOCS__SERVER__BIND", "127.0.0.1:0")
            .current_dir(dir.path());
        if let Ok(profile) = std::env::var("LLVM_PROFILE_FILE") {
            cmd.env("LLVM_PROFILE_FILE", profile);
        }
    };
    let binary = env!("CARGO_BIN_EXE_joedocs");
    let mut init = Command::new(binary);
    env(&mut init);
    assert!(init.arg("init").output().unwrap().status.success());

    let mut serve = Command::new(binary);
    env(&mut serve);
    let mut child = serve
        .arg("serve")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(
        line.starts_with("JoeDocs MCP endpoint: http://127.0.0.1:"),
        "{line}"
    );
    let addr: std::net::SocketAddr = line
        .trim()
        .trim_start_matches("JoeDocs MCP endpoint: http://")
        .trim_end_matches("/mcp")
        .parse()
        .unwrap();
    let health = format!("GET /health HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    assert_eq!(runtime.block_on(status(addr, &health)).unwrap(), 200);

    let killed = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    assert!(child.wait().unwrap().success(), "graceful shutdown exits 0");
}

#[test]
fn serve_reports_an_unusable_address() {
    let dir = TempDir::new().unwrap();
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let cmd = || {
        let mut cmd = assert_cmd::cargo::cargo_bin_cmd!();
        cmd.env_clear()
            .env("HOME", dir.path())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("JOEDOCS__STORAGE__HOME", dir.path().join("vault"))
            .env("JOEDOCS__STORAGE__STATE_DIR", dir.path().join("state"))
            .env(
                "JOEDOCS__SERVER__BIND",
                taken.local_addr().unwrap().to_string(),
            )
            .current_dir(dir.path());
        if let Ok(profile) = std::env::var("LLVM_PROFILE_FILE") {
            cmd.env("LLVM_PROFILE_FILE", profile);
        }
        cmd
    };
    cmd().arg("init").assert().success();
    cmd()
        .arg("serve")
        .assert()
        .code(69)
        .stderr(predicates::str::contains("cannot serve on 127.0.0.1:"));
}
