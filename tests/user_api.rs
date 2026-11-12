//! The admin API's user routes (0.5.1): password sign-in with TOTP setup at
//! the first sign-in, device credentials and their renewal, the signed-in
//! user's account and devices, one generic refusal, the per-name lock, and
//! one audit row per call that names the user and never a secret.

use std::sync::Arc;

use joedocs::Config;
use joedocs::auth::OsRandom;
use joedocs::clock::{Clock as _, FixedClock};
use joedocs::history::GitCli;
use joedocs::mcp::{AdminOptions, ServerOptions, Shared, serve};
use joedocs::store::{Paths, Settings, Store};
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// An audit row: tool, outcome, key name, user name.
type AuditRow = (String, String, String, Option<String>);

struct Server {
    dir: TempDir,
    addr: std::net::SocketAddr,
    clock: Arc<FixedClock>,
    /// The one-time password of each user created at start.
    initial: Vec<(String, String)>,
    /// An admin token, to show it gets nothing from the user routes.
    token: String,
    /// A session of that admin token, opened before the server started.
    token_session: String,
    /// An MCP key with write access to project `joedocs`.
    key: String,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

struct Reply {
    status: u16,
    head: String,
    body: String,
    json: Value,
}

async fn start(users: &[&str]) -> TestResult<Server> {
    start_with(users, true).await
}

async fn start_with(users: &[&str], token_login: bool) -> TestResult<Server> {
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
    store.add_project("joedocs", "JD", Some("JoeDocs"))?;
    let key = store.create_key("agent", &["joedocs:write".parse()?], &OsRandom)?;
    let token = store.create_admin_token("ops", &OsRandom)?;
    let token_secret =
        joedocs::totp::base32_decode(token.totp_secret.expose_secret()).ok_or("bad base32")?;
    let token_code = joedocs::totp::code(&token_secret, joedocs::totp::step_at(1_793_000_000));
    let token_session = store.admin_login(&token.token, &token_code, 8, &OsRandom)?;
    let mut initial = Vec::new();
    for name in users {
        let password = joedocs::password::initial_password(&OsRandom)?;
        let hash = joedocs::password::hash(&password, &OsRandom)?;
        store.create_user(name, &hash, &OsRandom)?;
        initial.push(((*name).to_owned(), password.expose_secret().to_owned()));
    }
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let mut options = ServerOptions::from_config(&Config::default());
    options.token_login = token_login;
    options.admin = Some(AdminOptions {
        session_hours: 8,
        config_file: None,
    });
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
        initial,
        token: token.token.expose_secret().to_owned(),
        token_session: token_session.token.expose_secret().to_owned(),
        key: key.token.expose_secret().to_owned(),
        stop: Some(stop),
        task,
    })
}

impl Server {
    fn initial(&self, name: &str) -> String {
        self.initial
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, p)| p.clone())
            .unwrap_or_default()
    }

    fn code(&self, secret: &[u8]) -> String {
        joedocs::totp::code(secret, joedocs::totp::step_at(self.clock.now().as_second()))
    }

    async fn send(&self, method: &str, path: &str, headers: &str, body: &str) -> TestResult<Reply> {
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n{headers}content-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            self.addr.port(),
            body.len()
        );
        let mut stream = TcpStream::connect(self.addr).await?;
        stream.write_all(request.as_bytes()).await?;
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await?;
        let text = String::from_utf8_lossy(&raw).into_owned();
        let (head, body) = text.split_once("\r\n\r\n").ok_or("no header end")?;
        let status = head.split(' ').nth(1).ok_or("no status")?.parse()?;
        Ok(Reply {
            status,
            head: head.to_owned(),
            body: body.to_owned(),
            json: serde_json::from_str(body).unwrap_or(Value::Null),
        })
    }

    async fn post(&self, path: &str, headers: &str, body: &Value) -> TestResult<Reply> {
        self.send("POST", path, headers, &body.to_string()).await
    }

    /// Takes `name` through the first sign-in: TOTP setup and a new
    /// password. Returns the TOTP secret and the signed-in reply.
    async fn set_up(&self, name: &str, password: &str) -> TestResult<(Vec<u8>, Reply)> {
        let started = self
            .post(
                "/admin/v1/auth/login",
                "",
                &json!({"user": name, "password": self.initial(name), "device": "laptop"}),
            )
            .await?;
        assert_eq!(started.status, 200, "{}", started.body);
        let setup = &started.json["setup"];
        let secret =
            joedocs::totp::base32_decode(setup["totp"]["secret"].as_str().ok_or("no secret")?)
                .ok_or("bad base32")?;
        let uri = setup["totp"]["uri"].as_str().ok_or("no uri")?;
        assert!(
            uri.starts_with(&format!("otpauth://totp/JoeDocs:{name}?")),
            "{uri}"
        );
        assert!(
            started.json.get("session").is_none(),
            "setup gives no session"
        );
        let done = self
            .post(
                "/admin/v1/auth/setup",
                "",
                &json!({
                    "ticket": setup["ticket"],
                    "code": self.code(&secret),
                    "password": password,
                    "device": "laptop",
                }),
            )
            .await?;
        assert_eq!(done.status, 200, "{}", done.body);
        self.clock.advance(30);
        Ok((secret, done))
    }

    fn user_headers(reply: &Reply) -> String {
        format!(
            "Authorization: Bearer {}\r\nJoeDocs-Session: {}\r\n",
            reply.json["device"]["credential"]
                .as_str()
                .unwrap_or_default(),
            reply.json["session"].as_str().unwrap_or_default()
        )
    }

    fn audit(&self) -> TestResult<Vec<AuditRow>> {
        let conn = rusqlite::Connection::open(self.dir.path().join("state/joedocs.db"))?;
        let mut stmt = conn.prepare(
            "SELECT tool, outcome, key_name, user_name FROM audit_log WHERE tool LIKE 'admin:%' ORDER BY rowid",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn database(&self) -> TestResult<Vec<u8>> {
        Ok(std::fs::read(self.dir.path().join("state/joedocs.db"))?)
    }

    async fn stop(mut self) -> TestResult<()> {
        if let Some(stop) = self.stop.take() {
            stop.send(()).ok();
        }
        (&mut self.task).await??;
        Ok(())
    }
}

const PASSWORD: &str = "a long passphrase of my own";

#[tokio::test]
async fn a_new_user_sets_up_totp_at_the_first_sign_in_then_signs_in_with_a_code() -> TestResult<()>
{
    let server = start(&["alice"]).await?;
    let (secret, done) = server.set_up("alice", PASSWORD).await?;
    assert_eq!(done.json["user"], "alice");
    assert!(
        done.json["session"]
            .as_str()
            .unwrap_or_default()
            .starts_with("jdu_")
    );
    let device = done.json["device"]["credential"]
        .as_str()
        .unwrap_or_default();
    assert!(device.starts_with("jdd_"), "{}", done.body);
    // The session works on the existing routes.
    let headers = Server::user_headers(&done);
    let status = server.send("GET", "/admin/v1/status", &headers, "").await?;
    assert_eq!(status.status, 200, "{}", status.body);
    let me = server.send("GET", "/admin/v1/me", &headers, "").await?;
    assert_eq!(
        (me.json["user"].as_str(), me.json["device"].as_str()),
        (Some("alice"), Some("laptop"))
    );
    // A later sign-in needs the password and a code.
    let again = server
        .post(
            "/admin/v1/auth/login",
            "",
            &json!({"user": "alice", "password": PASSWORD, "code": server.code(&secret), "device": "desktop"}),
        )
        .await?;
    assert_eq!(again.status, 200, "{}", again.body);
    assert_eq!(again.json["device"]["name"], "desktop");
    // Renewal with the device needs only a code.
    server.clock.advance(30);
    let renewed = server
        .post(
            "/admin/v1/session",
            &format!("Authorization: Bearer {device}\r\n"),
            &json!({"code": server.code(&secret)}),
        )
        .await?;
    assert_eq!(renewed.status, 200, "{}", renewed.body);
    assert_eq!(renewed.json["name"], "alice");
    let audit = server.audit()?;
    assert!(
        audit.contains(&(
            "admin:auth.login".into(),
            "ok".into(),
            "alice".into(),
            Some("alice".into())
        )),
        "{audit:?}"
    );
    assert!(
        audit.contains(&(
            "admin:auth.setup".into(),
            "ok".into(),
            "alice".into(),
            Some("alice".into())
        )),
        "{audit:?}"
    );
    assert!(
        audit.contains(&(
            "admin:session.create".into(),
            "ok".into(),
            "alice".into(),
            Some("alice".into())
        )),
        "{audit:?}"
    );
    server.stop().await
}

#[tokio::test]
async fn refusals_are_identical_and_never_say_which_part_was_wrong() -> TestResult<()> {
    let server = start(&["alice", "dave"]).await?;
    let (secret, _) = server.set_up("alice", PASSWORD).await?;
    let (dave, _) = server.set_up("dave", "dave's own long password").await?;
    let mut store_dir = server.dir.path().join("state");
    store_dir.push("joedocs.db");
    {
        // Disable dave directly in the database, as `joedocs user disable`
        // would on the server.
        let conn = rusqlite::Connection::open(&store_dir)?;
        conn.execute("UPDATE users SET disabled_at = 't' WHERE name = 'dave'", [])?;
    }
    let code = server.code(&secret);
    let attempts = [
        json!({"user": "nobody", "password": PASSWORD, "code": code}),
        json!({"user": "alice", "password": "not the password at all", "code": code}),
        json!({"user": "alice", "password": PASSWORD, "code": "000000"}),
        json!({"user": "dave", "password": "dave's own long password", "code": server.code(&dave)}),
    ];
    let mut bodies = Vec::new();
    for attempt in &attempts {
        let reply = server.post("/admin/v1/auth/login", "", attempt).await?;
        assert_eq!(reply.status, 401, "{}", reply.body);
        bodies.push(reply.body);
    }
    assert!(bodies.windows(2).all(|w| w[0] == w[1]), "{bodies:?}");
    let audit = server.audit()?;
    let refused: Vec<&AuditRow> = audit.iter().filter(|r| r.1 == "unauthorized").collect();
    assert_eq!(refused.len(), 4);
    // The unknown name is never written to the audit log.
    assert_eq!(refused[0].2, "unknown");
    assert!(!audit.iter().any(|r| r.2 == "nobody"));
    server.stop().await
}

#[tokio::test]
async fn five_failures_lock_a_name_without_checking_the_password_and_stop_nothing_else()
-> TestResult<()> {
    let server = start(&["alice"]).await?;
    let (secret, done) = server.set_up("alice", PASSWORD).await?;
    let headers = Server::user_headers(&done);
    for name in ["alice", "nobody"] {
        for _ in 0..5 {
            let wrong =
                json!({"user": name, "password": "a wrong long password", "code": "000000"});
            let reply = server.post("/admin/v1/auth/login", "", &wrong).await?;
            assert_eq!(reply.status, 401);
        }
    }
    let real = server
        .post(
            "/admin/v1/auth/login",
            "",
            &json!({"user": "alice", "password": PASSWORD, "code": server.code(&secret)}),
        )
        .await?;
    let unknown = server
        .post(
            "/admin/v1/auth/login",
            "",
            &json!({"user": "nobody", "password": PASSWORD}),
        )
        .await?;
    assert_eq!((real.status, unknown.status), (429, 429));
    assert_eq!(real.body, unknown.body);
    assert!(
        real.head.to_lowercase().contains("retry-after:"),
        "{}",
        real.head
    );
    // The open session and the MCP key keep working.
    let status = server.send("GET", "/admin/v1/status", &headers, "").await?;
    assert_eq!(status.status, 200);
    let mcp = server
        .send(
            "POST",
            "/mcp",
            &format!("Authorization: Bearer {}\r\nAccept: application/json, text/event-stream\r\n", server.key),
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
        )
        .await?;
    assert_ne!(mcp.status, 401, "the MCP key still works: {}", mcp.body);
    // And so does renewing the device.
    let device = done.json["device"]["credential"]
        .as_str()
        .unwrap_or_default();
    let renewed = server
        .post(
            "/admin/v1/session",
            &format!("Authorization: Bearer {device}\r\n"),
            &json!({"code": server.code(&secret)}),
        )
        .await?;
    assert_eq!(renewed.status, 200, "{}", renewed.body);
    server.stop().await
}

#[tokio::test]
async fn a_password_alone_and_a_pending_setup_open_nothing() -> TestResult<()> {
    let server = start(&["alice", "bob"]).await?;
    // A one-time password gives a setup ticket, never a session.
    let setup = server
        .post(
            "/admin/v1/auth/login",
            "",
            &json!({"user": "bob", "password": server.initial("bob"), "code": "123456"}),
        )
        .await?;
    assert_eq!(setup.status, 200);
    assert!(setup.json.get("session").is_none() && setup.json.get("device").is_none());
    // The ticket is not a session.
    let ticket = setup.json["setup"]["ticket"].as_str().unwrap_or_default();
    let as_session = server
        .send(
            "GET",
            "/admin/v1/status",
            &format!("Authorization: Bearer {ticket}\r\nJoeDocs-Session: {ticket}\r\n"),
            "",
        )
        .await?;
    assert_eq!(as_session.status, 401);
    // A wrong code confirms nothing; a weak password is refused with the reason.
    let wrong = server
        .post(
            "/admin/v1/auth/setup",
            "",
            &json!({"ticket": ticket, "code": "000000", "password": PASSWORD}),
        )
        .await?;
    assert_eq!(wrong.status, 401);
    let weak = server
        .post(
            "/admin/v1/auth/setup",
            "",
            &json!({"ticket": ticket, "code": "000000", "password": "short"}),
        )
        .await?;
    assert_eq!(weak.status, 400);
    assert!(weak.body.contains("too short"), "{}", weak.body);
    // Set up, then a password with no code is refused.
    server.set_up("alice", PASSWORD).await?;
    let no_code = server
        .post(
            "/admin/v1/auth/login",
            "",
            &json!({"user": "alice", "password": PASSWORD}),
        )
        .await?;
    assert_eq!(no_code.status, 401);
    server.stop().await
}

#[tokio::test]
async fn admin_tokens_get_nothing_from_the_user_routes() -> TestResult<()> {
    let server = start(&[]).await?;
    let token = format!("Authorization: Bearer {}\r\n", server.token);
    for (method, path) in [
        ("GET", "/admin/v1/me"),
        ("GET", "/admin/v1/me/devices"),
        ("POST", "/admin/v1/me/devices/x/revoke"),
    ] {
        let reply = server.send(method, path, &token, "{}").await?;
        assert_eq!(reply.status, 403, "{path}");
        assert_eq!(reply.json["error"]["code"], "user_required", "{path}");
    }
    let reply = server
        .post(
            "/admin/v1/me/password",
            &token,
            &json!({"password": "x", "code": "1", "new_password": "y"}),
        )
        .await?;
    assert_eq!(reply.json["error"]["code"], "user_required");
    server.stop().await
}

#[tokio::test]
async fn devices_are_listed_and_revoked_and_a_password_change_ends_the_others() -> TestResult<()> {
    let server = start(&["alice"]).await?;
    let (secret, laptop) = server.set_up("alice", PASSWORD).await?;
    let desktop = server
        .post("/admin/v1/auth/login", "", &json!({"user": "alice", "password": PASSWORD, "code": server.code(&secret), "device": "desktop"}))
        .await?;
    server.clock.advance(30);
    let headers = Server::user_headers(&laptop);
    let list = server
        .send("GET", "/admin/v1/me/devices", &headers, "")
        .await?;
    let output = list.json["output"].as_str().unwrap_or_default();
    assert!(
        output.contains("laptop (this one)") && output.contains("desktop"),
        "{output}"
    );
    let changed = server
        .post(
            "/admin/v1/me/password",
            &headers,
            &json!({"password": PASSWORD, "code": server.code(&secret), "new_password": "a brand new passphrase"}),
        )
        .await?;
    assert_eq!(changed.status, 200, "{}", changed.body);
    let desktop_headers = Server::user_headers(&desktop);
    let ended = server
        .send("GET", "/admin/v1/status", &desktop_headers, "")
        .await?;
    assert_eq!(ended.json["error"]["code"], "device_invalid");
    let still = server.send("GET", "/admin/v1/status", &headers, "").await?;
    assert_eq!(still.status, 200);
    let wrong = server
        .post(
            "/admin/v1/me/password",
            &headers,
            &json!({"password": "not my password at all", "code": "000000", "new_password": "another new passphrase"}),
        )
        .await?;
    assert_eq!(wrong.status, 401);
    let revoked = server
        .post("/admin/v1/me/devices/laptop/revoke", &headers, &json!({}))
        .await?;
    assert_eq!(revoked.status, 200, "{}", revoked.body);
    let gone = server.send("GET", "/admin/v1/me", &headers, "").await?;
    assert_eq!(gone.json["error"]["code"], "device_invalid");
    server.stop().await
}

#[tokio::test]
async fn no_password_code_ticket_or_credential_reaches_the_audit_log() -> TestResult<()> {
    let server = start(&["alice"]).await?;
    let (secret, done) = server.set_up("alice", PASSWORD).await?;
    // A password typed into the name field.
    let typo = server
        .post(
            "/admin/v1/auth/login",
            "",
            &json!({"user": PASSWORD, "password": "x", "code": "000000"}),
        )
        .await?;
    assert_eq!(typo.status, 401);
    let database = String::from_utf8_lossy(&server.database()?).into_owned();
    let audit = format!("{:?}", server.audit()?);
    for secret_text in [
        PASSWORD.to_owned(),
        server.initial("alice"),
        server.code(&secret),
        done.json["device"]["credential"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        done.json["session"].as_str().unwrap_or_default().to_owned(),
    ] {
        assert!(!audit.contains(&secret_text), "{secret_text}");
    }
    assert!(
        !database.contains(PASSWORD),
        "no password in the database file"
    );
    server.stop().await
}

fn last_line(reply: &Reply) -> String {
    reply.json["output"]
        .as_str()
        .unwrap_or_default()
        .lines()
        .last()
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn signed_in_users_manage_users_with_the_servers_own_output() -> TestResult<()> {
    let server = start(&["alice"]).await?;
    let (_, done) = server.set_up("alice", PASSWORD).await?;
    let headers = Server::user_headers(&done);
    let created = server
        .post("/admin/v1/users", &headers, &json!({"name": "bob"}))
        .await?;
    assert_eq!(created.status, 200, "{}", created.body);
    let output = created.json["output"].as_str().unwrap_or_default();
    assert!(output.starts_with("Created user bob.\n"), "{output}");
    let password = last_line(&created);
    assert_eq!(password.len(), 23, "{password}");
    // The new user can start their setup with it.
    let setup = server
        .post(
            "/admin/v1/auth/login",
            "",
            &json!({"user": "bob", "password": password}),
        )
        .await?;
    assert!(setup.json["setup"]["ticket"].is_string(), "{}", setup.body);
    let list = server.send("GET", "/admin/v1/users", &headers, "").await?;
    let output = list.json["output"].as_str().unwrap_or_default();
    assert!(
        output.contains("alice") && output.contains("bob"),
        "{output}"
    );
    for (action, says) in [
        ("disable", "Disabled user bob"),
        ("enable", "Enabled user bob"),
        ("unlock", "Cleared the sign-in lock on bob"),
        ("reset-password", "Reset the password of bob"),
        ("reset-totp", "Cleared the authenticator of bob"),
    ] {
        let reply = server
            .post(
                &format!("/admin/v1/users/bob/{action}"),
                &headers,
                &json!({}),
            )
            .await?;
        assert_eq!(reply.status, 200, "{action}: {}", reply.body);
        assert!(
            reply.json["output"]
                .as_str()
                .unwrap_or_default()
                .contains(says),
            "{action}"
        );
    }
    let missing = server
        .post("/admin/v1/users/nobody/disable", &headers, &json!({}))
        .await?;
    assert_eq!(missing.json["error"]["code"], "not_found");
    let remove = server
        .post("/admin/v1/users/bob/remove", &headers, &json!({}))
        .await?;
    assert_eq!(
        (remove.status, remove.json["error"]["code"].as_str()),
        (404, Some("not_found"))
    );
    let audit = server.audit()?;
    assert!(audit.contains(&(
        "admin:users.create".into(),
        "ok".into(),
        "alice".into(),
        Some("alice".into())
    )));
    assert!(
        audit
            .iter()
            .any(|r| r.0 == "admin:users.reset_totp" && r.1 == "ok")
    );
    server.stop().await
}

#[tokio::test]
async fn the_last_user_who_can_sign_in_cannot_be_disabled_or_reset_remotely() -> TestResult<()> {
    let server = start(&["alice", "bob"]).await?;
    let (_, done) = server.set_up("alice", PASSWORD).await?;
    let headers = Server::user_headers(&done);
    // Bob has not set up an authenticator, so alice is the only one who can
    // sign in.
    for action in ["disable", "reset-password", "reset-totp"] {
        let reply = server
            .post(
                &format!("/admin/v1/users/alice/{action}"),
                &headers,
                &json!({}),
            )
            .await?;
        assert_eq!(reply.status, 400, "{action}");
        assert!(
            reply.body.contains("last user who can sign in"),
            "{}",
            reply.body
        );
    }
    let still = server.send("GET", "/admin/v1/me", &headers, "").await?;
    assert_eq!(still.status, 200);
    server.stop().await
}

#[tokio::test]
async fn admin_tokens_cannot_manage_users_and_their_replies_are_marked_deprecated() -> TestResult<()>
{
    let server = start(&["alice"]).await?;
    let token = format!(
        "Authorization: Bearer {}\r\nJoeDocs-Session: {}\r\n",
        server.token, server.token_session
    );
    for (method, path) in [
        ("GET", "/admin/v1/users"),
        ("POST", "/admin/v1/users/alice/disable"),
    ] {
        let reply = server.send(method, path, &token, "{}").await?;
        assert_eq!(reply.status, 403, "{path}");
        assert_eq!(reply.json["error"]["code"], "user_required");
    }
    let created = server
        .post("/admin/v1/users", &token, &json!({"name": "eve"}))
        .await?;
    assert_eq!(created.json["error"]["code"], "user_required");
    let status = server.send("GET", "/admin/v1/status", &token, "").await?;
    assert_eq!(status.status, 200);
    let head = status.head.to_lowercase();
    assert!(head.contains("deprecation: @1794441600"), "{head}");
    assert!(head.contains("rel=\"deprecation\""), "{head}");
    let (_, done) = server.set_up("alice", PASSWORD).await?;
    let user = server
        .send("GET", "/admin/v1/status", &Server::user_headers(&done), "")
        .await?;
    assert!(
        !user.head.to_lowercase().contains("deprecation:"),
        "{}",
        user.head
    );
    server.stop().await
}

#[tokio::test]
async fn switching_token_login_off_refuses_every_admin_token_request() -> TestResult<()> {
    let server = start_with(&["alice"], false).await?;
    let token = format!(
        "Authorization: Bearer {}\r\nJoeDocs-Session: {}\r\n",
        server.token, server.token_session
    );
    let status = server.send("GET", "/admin/v1/status", &token, "").await?;
    assert_eq!(status.status, 401);
    assert!(status.body.contains("admin.token_login"), "{}", status.body);
    let login = server
        .post(
            "/admin/v1/session",
            &format!("Authorization: Bearer {}\r\n", server.token),
            &json!({"code": "123456"}),
        )
        .await?;
    assert_eq!(login.status, 401);
    assert!(login.body.contains("admin.token_login"), "{}", login.body);
    // Users are not affected.
    let (_, done) = server.set_up("alice", PASSWORD).await?;
    let user = server
        .send("GET", "/admin/v1/status", &Server::user_headers(&done), "")
        .await?;
    assert_eq!(user.status, 200);
    server.stop().await
}

#[tokio::test]
async fn the_audit_json_names_the_user_of_a_users_calls_only() -> TestResult<()> {
    let server = start(&["alice"]).await?;
    let (_, done) = server.set_up("alice", PASSWORD).await?;
    let headers = Server::user_headers(&done);
    let read = server
        .send("GET", "/admin/v1/audit?limit=100", &headers, "")
        .await?;
    assert_eq!(read.status, 200, "{}", read.body);
    let items = read.json["items"].as_array().ok_or("no items")?;
    let setup = items
        .iter()
        .find(|i| i["tool"] == "admin:auth.setup")
        .ok_or("no setup row")?;
    assert_eq!(setup["user"], "alice");
    let token = format!(
        "Authorization: Bearer {}\r\nJoeDocs-Session: {}\r\n",
        server.token, server.token_session
    );
    server.send("GET", "/admin/v1/status", &token, "").await?;
    let read = server
        .send("GET", "/admin/v1/audit?limit=100", &headers, "")
        .await?;
    let items = read.json["items"].as_array().ok_or("no items")?;
    let by_token = items
        .iter()
        .find(|i| i["tool"] == "admin:status" && i["key_name"] == "ops")
        .ok_or("no token row")?;
    assert!(by_token.get("user").is_none(), "{by_token}");
    server.stop().await
}
