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
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

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
        let mut stream = TcpStream::connect(self.addr).await?;
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n{headers}content-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            self.addr.port(),
            body.len()
        );
        stream.write_all(request.as_bytes()).await?;
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await?;
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
    let mut stream = TcpStream::connect(server.addr).await?;
    let request = format!(
        "GET /admin/v1/status HTTP/1.1\r\nHost: evil.example\r\nConnection: close\r\n{}\r\n",
        server.auth(Some(&session))
    );
    stream.write_all(request.as_bytes()).await?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await?;
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
