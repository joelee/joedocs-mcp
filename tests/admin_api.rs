//! The remote admin API (`/admin/v1/`) served by `joedocs serve`: off by
//! default, admin token plus TOTP login, sessions that expire, lockout,
//! `Host`/`Origin` checks and one audit row per call.

use std::path::Path;
use std::sync::{Arc, Mutex};

use joedocs::Config;
use joedocs::clock::{Clock as _, FixedClock};
use joedocs::history::GitCli;
use joedocs::mcp::{AdminOptions, ServerOptions, Shared, serve};
use joedocs::store::{Paths, Settings, Store};
use secrecy::ExposeSecret as _;
use serde_json::Value;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpSocket};
use tokio::sync::oneshot;

type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Sends `request` on a new connection and reads the whole reply. The send
/// buffer is small, so a large body is still being written when the server
/// answers, as on a busy network.
///
/// The server refuses an oversized body, or a bad `Host`, without reading
/// the rest of the body, and closes the connection. Writing on then fails
/// with a broken pipe or a reset, and reading may end with a reset, but the
/// reply has already arrived: that is what a client sees, so it is the
/// reply returned here.
async fn exchange(addr: std::net::SocketAddr, request: &[u8]) -> TestResult<Vec<u8>> {
    let closed_early = |e: &std::io::Error| {
        matches!(
            e.kind(),
            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
        )
    };
    let socket = TcpSocket::new_v4()?;
    socket.set_send_buffer_size(4096)?;
    let mut stream = socket.connect(addr).await?;
    if let Err(e) = stream.write_all(request).await
        && !closed_early(&e)
    {
        return Err(e.into());
    }
    let mut raw = Vec::new();
    if let Err(e) = stream.read_to_end(&mut raw).await
        && (raw.is_empty() || !closed_early(&e))
    {
        return Err(e.into());
    }
    Ok(raw)
}

struct Server {
    dir: TempDir,
    addr: std::net::SocketAddr,
    clock: Arc<FixedClock>,
    token: String,
    secret: Vec<u8>,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

async fn start(admin: bool) -> TestResult<Server> {
    let dir = TempDir::new()?;
    let paths = Paths {
        home: dir.path().join("vault"),
        state_dir: dir.path().join("state"),
    };
    let git = GitCli::new(paths.git_dir(), &paths.home);
    Store::init(&paths, &git)?;
    let clock = Arc::new(FixedClock::at(1_793_000_000));
    let mut store = Store::open(
        paths,
        Settings::from_config(&Config::default()),
        Box::new(git),
        Arc::clone(&clock) as Arc<dyn joedocs::clock::Clock>,
    )?;
    let created = store.create_admin_token("laptop", &joedocs::auth::OsRandom)?;
    let secret =
        joedocs::totp::base32_decode(created.totp_secret.expose_secret()).ok_or("bad base32")?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let mut options = ServerOptions::from_config(&Config::default());
    if admin {
        options.admin = Some(AdminOptions {
            session_hours: 8,
            config_file: None,
        });
    }
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        serve(listener, Shared::new(store), &options, async {
            stopped.await.ok();
        })
        .await
    });
    Ok(Server {
        dir,
        addr,
        clock,
        token: created.token.expose_secret().to_owned(),
        secret,
        stop: Some(stop),
        task,
    })
}

struct Response {
    status: u16,
    head: String,
    json: Value,
}

impl Server {
    fn code(&self) -> String {
        let step = joedocs::totp::step_at(self.clock.now().as_second());
        joedocs::totp::code(&self.secret, step)
    }

    /// Sends `method path` with extra header lines and an optional JSON body.
    async fn send(
        &self,
        method: &str,
        path: &str,
        headers: &str,
        body: &str,
    ) -> TestResult<Response> {
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n{headers}content-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            self.addr.port(),
            body.len()
        );
        let raw = exchange(self.addr, request.as_bytes()).await?;
        let text = String::from_utf8_lossy(&raw).into_owned();
        let (head, body) = text.split_once("\r\n\r\n").ok_or("no header end")?;
        let status = head.split(' ').nth(1).ok_or("no status")?.parse()?;
        let json = serde_json::from_str(body).unwrap_or(Value::Null);
        Ok(Response {
            status,
            head: head.to_owned(),
            json,
        })
    }

    fn auth(&self, session: Option<&str>) -> String {
        let session = session.map_or_else(String::new, |s| format!("JoeDocs-Session: {s}\r\n"));
        format!("Authorization: Bearer {}\r\n{session}", self.token)
    }

    async fn login(&self) -> TestResult<String> {
        let response = self
            .send(
                "POST",
                "/admin/v1/session",
                &self.auth(None),
                &format!(r#"{{"code":"{}"}}"#, self.code()),
            )
            .await?;
        assert_eq!(response.status, 200, "{:?}", response.json);
        Ok(response.json["session"]
            .as_str()
            .ok_or("no session")?
            .to_owned())
    }

    fn audit(&self) -> TestResult<Vec<(String, String)>> {
        let conn = rusqlite::Connection::open(self.dir.path().join("state/joedocs.db"))?;
        let mut stmt = conn.prepare(
            "SELECT tool, outcome FROM audit_log WHERE tool LIKE 'admin:%' ORDER BY rowid",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    async fn stop(mut self) -> TestResult<()> {
        if let Some(stop) = self.stop.take() {
            stop.send(()).ok();
        }
        (&mut self.task).await??;
        Ok(())
    }
}

/// Captures every log line written while the test runs.
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Ok(mut logs) = self.0.lock() {
            logs.extend_from_slice(buf);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn the_admin_api_is_off_by_default() -> TestResult<()> {
    let server = start(false).await?;
    let response = server
        .send("GET", "/admin/v1/status", &server.auth(None), "")
        .await?;
    assert_eq!(response.status, 404);
    let response = server
        .send(
            "POST",
            "/admin/v1/session",
            &server.auth(None),
            &format!(r#"{{"code":"{}"}}"#, server.code()),
        )
        .await?;
    assert_eq!(response.status, 404);
    assert_eq!(server.audit()?, Vec::new());
    server.stop().await
}

#[tokio::test]
async fn login_opens_a_session_that_reads_status_until_it_expires() -> TestResult<()> {
    let logs = Logs::default();
    let writer = logs.clone();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || writer.clone())
            .finish(),
    );
    let server = start(true).await?;
    let session = server.login().await?;
    assert!(session.starts_with("jds_"), "{session}");

    let status = server
        .send("GET", "/admin/v1/status", &server.auth(Some(&session)), "")
        .await?;
    assert_eq!(status.status, 200, "{:?}", status.json);
    assert_eq!(status.json["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(status.json["store"]["admin_tokens_active"], 1);

    // Without the session, or with only the session, it is refused.
    let missing = server
        .send("GET", "/admin/v1/status", &server.auth(None), "")
        .await?;
    assert_eq!(missing.status, 401);
    assert_eq!(missing.json["error"]["code"], "no_session");
    let no_token = server
        .send(
            "GET",
            "/admin/v1/status",
            &format!("JoeDocs-Session: {session}\r\n"),
            "",
        )
        .await?;
    assert_eq!(no_token.status, 401);
    assert_eq!(no_token.json["error"]["code"], "unauthorized");

    server.clock.advance(8 * 3600);
    let expired = server
        .send("GET", "/admin/v1/status", &server.auth(Some(&session)), "")
        .await?;
    assert_eq!(expired.status, 401);
    assert_eq!(expired.json["error"]["code"], "session_expired");

    let fresh = server.login().await?;
    let logout = server
        .send(
            "DELETE",
            "/admin/v1/session",
            &server.auth(Some(&fresh)),
            "",
        )
        .await?;
    assert_eq!(logout.status, 204);
    let after = server
        .send("GET", "/admin/v1/status", &server.auth(Some(&fresh)), "")
        .await?;
    assert_eq!(after.status, 401);

    let audit = server.audit()?;
    let outcomes: Vec<(&str, &str)> = audit
        .iter()
        .map(|(t, o)| (t.as_str(), o.as_str()))
        .collect();
    assert_eq!(
        outcomes,
        [
            ("admin:session.create", "ok"),
            ("admin:status", "ok"),
            ("admin:status", "no_session"),
            ("admin:status", "unauthorized"),
            ("admin:status", "session_expired"),
            ("admin:session.create", "ok"),
            ("admin:session.delete", "ok"),
            ("admin:status", "no_session"),
        ]
    );
    let text = String::from_utf8(logs.0.lock().map(|l| l.clone()).unwrap_or_default())?;
    assert!(!text.contains(&server.token), "the admin token was logged");
    assert!(
        !text.contains(&session) && !text.contains(&fresh),
        "a session was logged"
    );
    server.stop().await
}

#[tokio::test]
async fn wrong_codes_are_refused_and_lock_the_token() -> TestResult<()> {
    let server = start(true).await?;
    for _ in 0..5 {
        let refused = server
            .send(
                "POST",
                "/admin/v1/session",
                &server.auth(None),
                r#"{"code":"000000"}"#,
            )
            .await?;
        assert_eq!(refused.status, 401);
        assert_eq!(refused.json["error"]["code"], "unauthorized");
    }
    let locked = server
        .send(
            "POST",
            "/admin/v1/session",
            &server.auth(None),
            &format!(r#"{{"code":"{}"}}"#, server.code()),
        )
        .await?;
    assert_eq!(locked.status, 429);
    assert_eq!(locked.json["error"]["code"], "locked");
    assert!(
        locked.head.to_ascii_lowercase().contains("retry-after: "),
        "{}",
        locked.head
    );

    let unknown = server
        .send(
            "POST",
            "/admin/v1/session",
            "Authorization: Bearer jda_000000000000_0000\r\n",
            r#"{"code":"123456"}"#,
        )
        .await?;
    assert_eq!(unknown.status, 401);
    let audit = server.audit()?;
    assert_eq!(audit.len(), 7);
    assert_eq!(audit[5].1, "locked");
    assert_eq!(audit[6].1, "unauthorized");
    server.stop().await
}

#[tokio::test]
async fn foreign_hosts_and_origins_are_refused() -> TestResult<()> {
    let server = start(true).await?;
    let session = server.login().await?;
    let request = format!(
        "GET /admin/v1/status HTTP/1.1\r\nHost: evil.example\r\nConnection: close\r\n{}\r\n",
        server.auth(Some(&session))
    );
    let raw = exchange(server.addr, request.as_bytes()).await?;
    assert!(
        String::from_utf8_lossy(&raw).starts_with("HTTP/1.1 403"),
        "{}",
        String::from_utf8_lossy(&raw)
    );
    let origin = server
        .send(
            "GET",
            "/admin/v1/status",
            &format!(
                "{}Origin: https://evil.example\r\n",
                server.auth(Some(&session))
            ),
            "",
        )
        .await?;
    assert_eq!(origin.status, 403);
    assert_eq!(origin.json["error"]["code"], "forbidden");
    let audit = server.audit()?;
    assert_eq!(audit.last().map(|(_, o)| o.as_str()), Some("forbidden"));
    assert_eq!(audit.len(), 3);
    server.stop().await
}

#[test]
fn the_server_paths_are_where_the_test_expects() {
    // Guards the audit helper above against a renamed database file.
    assert_eq!(
        Paths {
            home: "/v".into(),
            state_dir: "/s".into()
        }
        .db(),
        Path::new("/s/joedocs.db")
    );
}

#[tokio::test]
async fn rejected_requests_are_audited_too() -> TestResult<()> {
    let server = start(true).await?;
    let session = server.login().await?;
    let before = server.audit()?.len();
    // Over the 64 KiB limit, and over axum's own 2 MB default.
    for size in [70 * 1024, 3 * 1024 * 1024] {
        let big = format!(r#"{{"code":"{}"}}"#, "1".repeat(size));
        let login = server
            .send("POST", "/admin/v1/session", &server.auth(None), &big)
            .await?;
        assert_eq!(login.status, 413, "{size}");
        assert_eq!(login.json["error"]["code"], "payload_too_large");
        let add = server
            .send(
                "POST",
                "/admin/v1/projects",
                &server.auth(Some(&session)),
                &big,
            )
            .await?;
        assert_eq!(add.status, 413);
    }
    // A bad Host is refused first, even with a huge body.
    let big = "1".repeat(70 * 1024);
    let request = format!(
        "POST /admin/v1/session HTTP/1.1\r\nHost: evil.example\r\nConnection: close\r\n{}content-length: {}\r\n\r\n{big}",
        server.auth(None),
        big.len()
    );
    let raw = exchange(server.addr, request.as_bytes()).await?;
    assert!(String::from_utf8_lossy(&raw).starts_with("HTTP/1.1 403"));
    let unknown = server
        .send("GET", "/admin/v1/nope", &server.auth(Some(&session)), "")
        .await?;
    assert_eq!(unknown.status, 404);
    assert_eq!(unknown.json["error"]["code"], "not_found");
    let wrong_method = server
        .send("PUT", "/admin/v1/status", &server.auth(Some(&session)), "")
        .await?;
    assert_eq!(wrong_method.status, 404);

    let audit = server.audit()?;
    let new: Vec<&str> = audit[before..].iter().map(|(_, o)| o.as_str()).collect();
    assert_eq!(
        new,
        [
            "payload_too_large",
            "payload_too_large",
            "payload_too_large",
            "payload_too_large",
            "forbidden",
            "not_found",
            "not_found",
        ]
    );
    let conn = rusqlite::Connection::open(server.dir.path().join("state/joedocs.db"))?;
    let leaked: i64 = conn.query_row(
        "SELECT COUNT(*) FROM audit_log WHERE key_name LIKE '%jd%' OR client LIKE '%jda_%' OR client LIKE '%jds_%' OR tool LIKE '%jd%_%'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(leaked, 0);
    server.stop().await
}

#[tokio::test]
async fn document_routes_are_audited_by_operation() -> TestResult<()> {
    let server = start(true).await?;
    let session = server.login().await?;
    let auth = server.auth(Some(&session));
    let convert = server
        .send("POST", "/admin/v1/documents/JD-1/convert", &auth, "")
        .await?;
    assert_eq!(convert.status, 404, "{:?}", convert.json);
    let no_file = server
        .send(
            "POST",
            "/admin/v1/documents/JD-1/files/move",
            &auth,
            r#"{"to":"JD-2"}"#,
        )
        .await?;
    assert_eq!(no_file.status, 400);
    assert!(
        no_file.json["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("`file` is required")
    );
    let no_version = server
        .send(
            "POST",
            "/admin/v1/documents/JD-1/files/restore",
            &auth,
            r#"{"file":"a","version":"two"}"#,
        )
        .await?;
    assert_eq!(no_version.status, 400);
    assert!(
        no_version.json["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("`version`")
    );
    let audit = server.audit()?;
    assert_eq!(
        audit[audit.len() - 3..],
        [
            ("admin:document.convert".to_owned(), "not_found".to_owned()),
            (
                "admin:document.move_file".to_owned(),
                "invalid_argument".to_owned()
            ),
            (
                "admin:document.restore_file".to_owned(),
                "invalid_argument".to_owned()
            ),
        ]
    );
    server.stop().await
}

#[tokio::test]
async fn the_documents_list_route_returns_json_and_is_audited() -> TestResult<()> {
    let server = start(true).await?;
    let no_session = server
        .send(
            "GET",
            "/admin/v1/documents?project=p",
            &server.auth(None),
            "",
        )
        .await?;
    assert_eq!(no_session.status, 401, "{:?}", no_session.json);
    let session = server.login().await?;
    let auth = server.auth(Some(&session));
    let added = server
        .send(
            "POST",
            "/admin/v1/projects",
            &auth,
            r#"{"name":"p","key":"P"}"#,
        )
        .await?;
    assert_eq!(added.status, 200, "{:?}", added.json);
    let listed = server
        .send(
            "GET",
            "/admin/v1/documents?project=p&sort=title&limit=5",
            &auth,
            "",
        )
        .await?;
    assert_eq!(listed.status, 200, "{:?}", listed.json);
    assert!(listed.head.contains("application/json"), "{}", listed.head);
    assert_eq!(
        listed.json,
        serde_json::json!({"items": [], "total": 0, "next_start": null})
    );
    for (path, needle) in [
        ("/admin/v1/documents", "`project` is required"),
        (
            "/admin/v1/documents?project=p&limit=many",
            "`limit` must be a number",
        ),
        (
            "/admin/v1/documents?project=p&sort=newest",
            "`sort` cannot be",
        ),
        ("/admin/v1/documents?project=p&type=nope", "is not one of"),
    ] {
        let bad = server.send("GET", path, &auth, "").await?;
        assert_eq!(bad.status, 400, "{path}: {:?}", bad.json);
        assert_eq!(bad.json["error"]["code"], "invalid_argument");
        assert!(
            bad.json["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains(needle),
            "{path}: {:?}",
            bad.json
        );
    }
    let unknown = server
        .send("GET", "/admin/v1/documents?project=nope", &auth, "")
        .await?;
    assert_eq!(unknown.status, 404);
    let audit = server.audit()?;
    let listing: Vec<&str> = audit
        .iter()
        .filter(|(tool, _)| tool == "admin:documents.list")
        .map(|(_, outcome)| outcome.as_str())
        .collect();
    assert_eq!(
        listing,
        [
            "no_session",
            "ok",
            "invalid_argument",
            "invalid_argument",
            "invalid_argument",
            "invalid_argument",
            "not_found"
        ]
    );
    server.stop().await
}
