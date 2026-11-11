//! The admin web interface (`/admin/`) served by `joedocs serve`: off by
//! default, a login page that opens a web-only session in a cookie, CSRF and
//! same-origin checks on every `POST`, security headers, hashed static
//! files, and one audit row per request.

use std::sync::Arc;

use joedocs::Config;
use joedocs::clock::{Clock as _, FixedClock};
use joedocs::history::GitCli;
use joedocs::mcp::{ServerOptions, Shared, serve};
use joedocs::store::{Paths, Settings, Store};
use secrecy::ExposeSecret as _;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// The Content-Security-Policy every page sends when no `PlantUML` server is
/// set.
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'";

struct Server {
    dir: TempDir,
    addr: std::net::SocketAddr,
    clock: Arc<FixedClock>,
    token: String,
    secret: Vec<u8>,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

/// Starts a server with `edit` applied to a configuration that has the admin
/// API and the web interface on.
async fn start_with(edit: impl FnOnce(&mut Config)) -> TestResult<Server> {
    start_full(edit, |_| Ok(())).await
}

/// As [`start_with`], with `setup` run on the store before it is served.
async fn start_full(
    edit: impl FnOnce(&mut Config),
    setup: impl FnOnce(&mut Store) -> TestResult<()>,
) -> TestResult<Server> {
    let mut config = Config::default();
    config.admin.enabled = true;
    config.admin.web_ui = true;
    edit(&mut config);
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
        Settings::from_config(&config),
        Box::new(git),
        Arc::clone(&clock) as Arc<dyn joedocs::clock::Clock>,
    )?;
    let created = store.create_admin_token("laptop", &joedocs::auth::OsRandom)?;
    setup(&mut store)?;
    let secret =
        joedocs::totp::base32_decode(created.totp_secret.expose_secret()).ok_or("bad base32")?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let options = ServerOptions::from_config(&config);
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

async fn start() -> TestResult<Server> {
    start_with(|_| {}).await
}

struct Reply {
    status: u16,
    head: String,
    body: String,
}

impl Reply {
    /// The value of the first `name` header, compared without case.
    fn header(&self, name: &str) -> Option<&str> {
        self.head.lines().skip(1).find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case(name)
                .then_some(value.trim())
        })
    }

    /// Every `Set-Cookie` value.
    fn cookies(&self) -> Vec<&str> {
        self.head
            .lines()
            .filter_map(|line| line.split_once(':'))
            .filter(|(key, _)| key.trim().eq_ignore_ascii_case("set-cookie"))
            .map(|(_, value)| value.trim())
            .collect()
    }

    /// The value of the hidden form field `name`.
    fn field(&self, name: &str) -> Option<String> {
        let marker = format!("name=\"{name}\" value=\"");
        let start = self.body.find(&marker)? + marker.len();
        let end = self.body[start..].find('"')?;
        Some(self.body[start..start + end].replace("&#x2f;", "/"))
    }
}

/// Encodes `pairs` as a form body.
fn form(pairs: &[(&str, &str)]) -> String {
    let mut out = form_urlencoded::Serializer::new(String::new());
    for (key, value) in pairs {
        out.append_pair(key, value);
    }
    out.finish()
}

impl Server {
    fn code(&self) -> String {
        let step = joedocs::totp::step_at(self.clock.now().as_second());
        joedocs::totp::code(&self.secret, step)
    }

    fn host(&self) -> String {
        format!("127.0.0.1:{}", self.addr.port())
    }

    fn origin(&self) -> String {
        format!("http://{}", self.host())
    }

    /// Sends `method path` with `Host: host`, extra header lines and a form
    /// body, and reads the whole reply.
    async fn send_to(
        &self,
        host: &str,
        method: &str,
        path: &str,
        headers: &str,
        body: &str,
    ) -> TestResult<Reply> {
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n{headers}content-type: application/x-www-form-urlencoded\r\ncontent-length: {}\r\n\r\n{body}",
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
        })
    }

    async fn send(&self, method: &str, path: &str, headers: &str, body: &str) -> TestResult<Reply> {
        self.send_to(&self.host(), method, path, headers, body)
            .await
    }

    async fn get(&self, path: &str, cookie: Option<&str>) -> TestResult<Reply> {
        let headers = cookie.map_or_else(String::new, |c| format!("Cookie: {c}\r\n"));
        self.send("GET", path, &headers, "").await
    }

    /// Posts the login form from this server's own origin.
    async fn post_login(&self, code: &str, next: &str) -> TestResult<Reply> {
        let body = form(&[("token", &self.token), ("code", code), ("next", next)]);
        let headers = format!(
            "Origin: {}\r\nSec-Fetch-Site: same-origin\r\n",
            self.origin()
        );
        self.send("POST", "/admin/login", &headers, &body).await
    }

    /// Logs in and returns the `name=value` cookie.
    async fn login(&self) -> TestResult<String> {
        let reply = self.post_login(&self.code(), "/admin/").await?;
        assert_eq!(reply.status, 303, "{}", reply.body);
        let cookie = reply.cookies().first().ok_or("no cookie")?.to_owned();
        Ok(cookie.split(';').next().ok_or("empty cookie")?.to_owned())
    }

    /// The CSRF token on the home page for `cookie`.
    async fn csrf(&self, cookie: &str) -> TestResult<String> {
        let page = self.get("/admin/", Some(cookie)).await?;
        assert_eq!(page.status, 200, "{}", page.body);
        Ok(page.field("csrf").ok_or("no csrf field")?)
    }

    /// `(tool, client, outcome)` of every audit row, oldest first.
    fn audit(&self) -> TestResult<Vec<(String, String, String)>> {
        let conn = rusqlite::Connection::open(self.dir.path().join("state/joedocs.db"))?;
        let mut stmt =
            conn.prepare("SELECT tool, client, outcome FROM audit_log ORDER BY rowid")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
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

#[tokio::test]
async fn the_web_interface_is_absent_when_switched_off() -> TestResult<()> {
    let server = start_with(|c| c.admin.web_ui = false).await?;
    let unknown = server.get("/no-such-path", None).await?;
    assert_eq!(unknown.status, 401, "0.4.3 answers unknown paths with 401");
    for path in ["/", "/admin", "/admin/", "/admin/login"] {
        let reply = server.get(path, None).await?;
        assert_eq!(reply.status, unknown.status, "{path}");
        assert_eq!(reply.body, unknown.body, "{path}");
    }
    let api = server.get("/admin/v1/status", None).await?;
    assert_eq!(api.status, 401);
    assert!(api.body.contains("\"unauthorized\""), "{}", api.body);
    server.stop().await
}

#[tokio::test]
async fn pages_need_a_session_and_next_stays_under_admin() -> TestResult<()> {
    let server = start().await?;
    let root = server.get("/", None).await?;
    assert_eq!(root.status, 303);
    assert_eq!(root.header("location"), Some("/admin/"));
    let home = server.get("/admin/", None).await?;
    assert_eq!(home.status, 303);
    assert_eq!(
        home.header("location"),
        Some("/admin/login?next=%2Fadmin%2F")
    );
    let page = server
        .get("/admin/login?next=%2Fadmin%2Fdocuments", None)
        .await?;
    assert_eq!(page.status, 200);
    assert_eq!(page.field("next").as_deref(), Some("/admin/documents"));
    for evil in [
        "https://evil.example/",
        "//evil.example/admin/",
        "/admin/../x",
        "/other",
    ] {
        let path = format!(
            "/admin/login?next={}",
            form(&[("x", evil)]).trim_start_matches("x=")
        );
        let page = server.get(&path, None).await?;
        assert_eq!(page.field("next").as_deref(), Some("/admin/"), "{evil}");
    }
    server.stop().await
}

#[tokio::test]
async fn login_sets_a_web_session_cookie_and_never_returns_the_token() -> TestResult<()> {
    let server = start().await?;
    let reply = server.post_login(&server.code(), "/admin/").await?;
    assert_eq!(reply.status, 303, "{}", reply.body);
    assert_eq!(reply.header("location"), Some("/admin/"));
    let cookies = reply.cookies();
    assert_eq!(cookies.len(), 1, "{cookies:?}");
    let cookie = cookies[0];
    assert!(cookie.starts_with("joedocs_session=jdw_"), "{cookie}");
    for part in ["HttpOnly", "SameSite=Strict", "Path=/", "Max-Age=28800"] {
        assert!(cookie.contains(part), "{part} in {cookie}");
    }
    assert!(!cookie.contains("Secure"), "plain loopback HTTP: {cookie}");
    assert!(!reply.head.contains(&server.token) && !reply.body.contains(&server.token));
    let session = cookie.split(';').next().ok_or("empty")?;
    let home = server.get("/admin/", Some(session)).await?;
    assert_eq!(home.status, 200, "{}", home.body);
    assert!(home.body.contains("laptop"), "{}", home.body);
    assert!(!home.body.contains(&server.token));

    // Over HTTPS (here, an https Origin naming this host), the cookie is
    // Secure and has the __Host- prefix.
    server.clock.advance(60);
    let body = form(&[("token", &server.token), ("code", &server.code())]);
    let headers = format!("Origin: https://{}\r\n", server.host());
    let reply = server.send("POST", "/admin/login", &headers, &body).await?;
    assert_eq!(reply.status, 303, "{}", reply.body);
    let cookie = reply.cookies()[0].to_owned();
    assert!(
        cookie.starts_with("__Host-joedocs_session=jdw_"),
        "{cookie}"
    );
    assert!(cookie.contains("Secure"), "{cookie}");
    let session = cookie.split(';').next().ok_or("empty")?;
    assert_eq!(server.get("/admin/", Some(session)).await?.status, 200);
    server.stop().await
}

#[tokio::test]
async fn login_over_plain_http_is_refused_off_loopback() -> TestResult<()> {
    let server = start_with(|c| c.server.allowed_hosts.push("docs.example.net".into())).await?;
    let body = form(&[("token", &server.token), ("code", &server.code())]);
    let reply = server
        .send_to(
            "docs.example.net",
            "POST",
            "/admin/login",
            "Origin: http://docs.example.net\r\n",
            &body,
        )
        .await?;
    assert_eq!(reply.status, 400, "{}", reply.body);
    assert_eq!(reply.cookies(), Vec::<&str>::new());
    assert!(reply.body.contains("HTTPS"), "{}", reply.body);
    server.stop().await
}

#[tokio::test]
async fn wrong_codes_show_the_error_then_the_lock() -> TestResult<()> {
    let server = start().await?;
    for _ in 0..5 {
        let reply = server.post_login("000000", "/admin/").await?;
        assert_eq!(reply.status, 401, "{}", reply.body);
        assert!(reply.body.contains("not valid"), "{}", reply.body);
        assert_eq!(reply.cookies(), Vec::<&str>::new());
    }
    let reply = server.post_login(&server.code(), "/admin/").await?;
    assert_eq!(reply.status, 429, "{}", reply.body);
    assert!(reply.body.contains("locked"), "{}", reply.body);
    assert!(reply.body.contains("try again after"), "{}", reply.body);
    assert!(reply.header("retry-after").is_some());
    server.stop().await
}

#[tokio::test]
async fn posts_need_the_csrf_token_and_the_same_origin() -> TestResult<()> {
    let server = start_with(|c| {
        c.server
            .allowed_origins
            .push("https://proxy.example.net".into());
    })
    .await?;
    let cookie = server.login().await?;
    let csrf = server.csrf(&cookie).await?;
    let own = server.origin();
    let refused = [
        (format!("Origin: {own}\r\n"), String::new()),
        (format!("Origin: {own}\r\n"), "csrf=wrong".to_owned()),
        (
            format!("Origin: {own}\r\nSec-Fetch-Site: cross-site\r\n"),
            format!("csrf={csrf}"),
        ),
        (
            "Origin: https://evil.example\r\n".to_owned(),
            format!("csrf={csrf}"),
        ),
        ("Origin: null\r\n".to_owned(), format!("csrf={csrf}")),
    ];
    for (origin, body) in &refused {
        let headers = format!("Cookie: {cookie}\r\n{origin}");
        let reply = server.send("POST", "/admin/logout", &headers, body).await?;
        assert_eq!(reply.status, 403, "{origin} {body}: {}", reply.body);
        // Nothing changed: the session still works.
        assert_eq!(server.get("/admin/", Some(&cookie)).await?.status, 200);
    }
    // An Origin listed in server.allowed_origins is accepted (a proxy that
    // rewrites Host).
    let headers = format!("Cookie: {cookie}\r\nOrigin: https://proxy.example.net\r\n");
    let reply = server
        .send("POST", "/admin/logout", &headers, &format!("csrf={csrf}"))
        .await?;
    assert_eq!(reply.status, 303, "{}", reply.body);
    server.stop().await
}

#[tokio::test]
async fn a_host_outside_allowed_hosts_is_refused() -> TestResult<()> {
    let server = start().await?;
    let reply = server
        .send_to("evil.example", "GET", "/admin/login", "", "")
        .await?;
    assert_eq!(reply.status, 403);
    let asset = server
        .send_to("evil.example", "GET", "/admin/assets/0/app.css", "", "")
        .await?;
    assert_eq!(asset.status, 403);
    server.stop().await
}

#[tokio::test]
async fn replies_carry_the_security_headers_and_assets_are_cached() -> TestResult<()> {
    let server = start().await?;
    let page = server.get("/admin/login", None).await?;
    assert_eq!(page.status, 200);
    assert_eq!(page.header("content-security-policy"), Some(CSP));
    assert_eq!(page.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(page.header("referrer-policy"), Some("same-origin"));
    assert_eq!(page.header("cache-control"), Some("no-store"));
    assert!(
        page.header("content-type")
            .is_some_and(|t| t.starts_with("text/html")),
        "{:?}",
        page.header("content-type")
    );
    assert!(page.body.contains("<html lang=\"en\""), "{}", page.body);
    assert!(!page.body.contains("<script>"), "no inline script");
    assert!(!page.body.contains("hx-on"), "no hx-on");
    for (name, kind) in [
        ("app.css", "text/css"),
        ("app.js", "text/javascript"),
        ("htmx.min.js", "text/javascript"),
    ] {
        let marker = "/admin/assets/";
        let at = page
            .body
            .find(&format!("/{name}\""))
            .ok_or(format!("{name} not linked"))?;
        let start = page.body[..at].rfind(marker).ok_or("no asset path")?;
        let path = &page.body[start..=(at + name.len())];
        let asset = server.get(path, None).await?;
        assert_eq!(asset.status, 200, "{path}");
        assert!(
            asset
                .header("content-type")
                .is_some_and(|t| t.starts_with(kind)),
            "{path}: {:?}",
            asset.header("content-type")
        );
        assert_eq!(
            asset.header("cache-control"),
            Some("public, max-age=31536000, immutable")
        );
        assert_eq!(asset.header("x-content-type-options"), Some("nosniff"));
        let stale = server
            .get(&format!("/admin/assets/0000000000000000/{name}"), None)
            .await?;
        assert_eq!(stale.status, 404, "{name} with a wrong hash");
    }
    server.stop().await
}

#[tokio::test]
async fn logout_ends_the_session_and_an_expired_one_returns_to_login() -> TestResult<()> {
    let server = start_with(|c| c.admin.session_hours = 1).await?;
    let cookie = server.login().await?;
    let csrf = server.csrf(&cookie).await?;
    let headers = format!("Cookie: {cookie}\r\nOrigin: {}\r\n", server.origin());
    let reply = server
        .send("POST", "/admin/logout", &headers, &format!("csrf={csrf}"))
        .await?;
    assert_eq!(reply.status, 303);
    assert_eq!(reply.header("location"), Some("/admin/login"));
    assert!(
        reply.cookies().iter().all(|c| c.contains("Max-Age=0")),
        "{:?}",
        reply.cookies()
    );
    let after = server.get("/admin/", Some(&cookie)).await?;
    assert_eq!(after.status, 303);

    server.clock.advance(60);
    let cookie = server.login().await?;
    server.clock.advance(3600);
    let expired = server.get("/admin/", Some(&cookie)).await?;
    assert_eq!(expired.status, 303);
    assert_eq!(
        expired.header("location"),
        Some("/admin/login?next=%2Fadmin%2F")
    );
    let headers = format!("Cookie: {cookie}\r\nHX-Request: true\r\n");
    let htmx = server.send("GET", "/admin/", &headers, "").await?;
    assert_eq!(
        htmx.header("hx-redirect"),
        Some("/admin/login?next=%2Fadmin%2F")
    );
    server.stop().await
}

#[tokio::test]
async fn web_requests_are_audited_with_the_web_client() -> TestResult<()> {
    let server = start().await?;
    let cookie = server.login().await?;
    let csrf = server.csrf(&cookie).await?;
    let headers = format!("Cookie: {cookie}\r\nOrigin: {}\r\n", server.origin());
    let refused = server.send("POST", "/admin/logout", &headers, "").await?;
    assert_eq!(refused.status, 403);
    let reply = server
        .send("POST", "/admin/logout", &headers, &format!("csrf={csrf}"))
        .await?;
    assert_eq!(reply.status, 303);
    // Static files are not audited.
    let page = server.get("/admin/login", None).await?;
    let css = page.body.find("/app.css\"").ok_or("no css")?;
    let start = page.body[..css].rfind("/admin/assets/").ok_or("no asset")?;
    server.get(&page.body[start..css + 8], None).await?;

    let rows = server.audit()?;
    let web: Vec<_> = rows
        .iter()
        .filter(|(_, client, _)| client == "joedocs-web")
        .map(|(tool, _, outcome)| (tool.as_str(), outcome.as_str()))
        .collect();
    assert_eq!(rows.len(), web.len(), "every row is from the web: {rows:?}");
    assert_eq!(
        web,
        [
            ("admin:session.create", "ok"),
            ("admin:status", "ok"),
            ("admin:session.delete", "forbidden"),
            ("admin:session.delete", "ok"),
            ("admin:web.login_page", "ok"),
        ]
    );
    let conn = rusqlite::Connection::open(server.dir.path().join("state/joedocs.db"))?;
    let names: Vec<String> = conn
        .prepare("SELECT key_name FROM audit_log ORDER BY rowid")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    assert_eq!(names[..4], ["laptop", "laptop", "laptop", "laptop"]);
    server.stop().await
}

// --- STEP-04: shell, design system and Overview -------------------------------

/// The pages every signed-in page links to, with their navigation labels.
const NAVIGATION: [(&str, &str); 7] = [
    ("/admin/", "Overview"),
    ("/admin/documents", "Documents"),
    ("/admin/projects", "Projects"),
    ("/admin/keys", "API keys"),
    ("/admin/audit", "Audit"),
    ("/admin/archive", "Archive"),
    ("/admin/maintenance", "Maintenance"),
];

#[tokio::test]
async fn the_overview_shows_the_server_its_projects_and_recent_activity() -> TestResult<()> {
    let server = start_full(
        |_| {},
        |store| {
            store.add_project("joedocs", "JD", Some("JoeDocs"))?;
            store.add_project("notes", "NT", None)?;
            // Clients name themselves; their names reach the page escaped.
            store.audit(
                "k1",
                &joedocs::store::Actor::new("laptop-key", "<script>alert(1)</script>"),
                "new_document",
                Some("JD-1"),
                "ok",
            )?;
            Ok(())
        },
    )
    .await?;
    let cookie = server.login().await?;
    let page = server.get("/admin/", Some(&cookie)).await?;
    assert_eq!(page.status, 200, "{}", page.body);
    for expected in [
        concat!("JoeDocs ", env!("CARGO_PKG_VERSION")),
        "joedocs",
        "JD",
        "notes",
        "NT",
        "Index",
        "new_document",
        "laptop-key",
        "Signed in as",
    ] {
        assert!(page.body.contains(expected), "{expected} in {}", page.body);
    }
    assert!(
        !page.body.contains("<script>alert"),
        "escaped: {}",
        page.body
    );
    assert!(
        page.body.contains("&#60;script&#62;alert(1)"),
        "{}",
        page.body
    );
    assert!(
        page.body
            .contains("href=\"/admin/documents?project=joedocs\""),
        "a project links to its documents"
    );
    assert!(
        page.body.contains("href=\"/admin/documents/JD-1\""),
        "an audit row links to its document"
    );
    for (href, label) in NAVIGATION {
        let link = format!("href=\"{href}\"");
        assert!(page.body.contains(&link), "{link} in the navigation");
        assert!(page.body.contains(label), "{label}");
    }
    assert!(page.body.contains("aria-current=\"page\""));
    assert!(
        page.body.contains("action=\"/admin/search\""),
        "search form"
    );
    assert!(
        page.body.contains("action=\"/admin/logout\""),
        "sign out form"
    );
    // The page's links all stay inside the interface or on this site.
    for (at, _) in page.body.match_indices("href=\"") {
        let rest = &page.body[at + 6..];
        assert!(
            rest.starts_with('/') || rest.starts_with('#'),
            "{}",
            &rest[..40.min(rest.len())]
        );
    }
    server.stop().await
}

#[test]
fn serve_prints_the_web_interface_url_only_when_it_is_on() {
    use std::io::{BufRead as _, BufReader};
    use std::process::{Command, Stdio};

    let dir = TempDir::new().unwrap();
    let binary = env!("CARGO_BIN_EXE_joedocs");
    let command = |web: bool| {
        let mut cmd = Command::new(binary);
        cmd.env_clear()
            .env("HOME", dir.path())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("JOEDOCS__STORAGE__HOME", dir.path().join("vault"))
            .env("JOEDOCS__STORAGE__STATE_DIR", dir.path().join("state"))
            .env("JOEDOCS__SERVER__BIND", "127.0.0.1:0")
            .env("JOEDOCS__ADMIN__ENABLED", "true")
            .env("JOEDOCS__ADMIN__WEB_UI", web.to_string())
            .current_dir(dir.path());
        if let Ok(profile) = std::env::var("LLVM_PROFILE_FILE") {
            cmd.env("LLVM_PROFILE_FILE", profile);
        }
        cmd
    };
    assert!(
        command(false)
            .arg("init")
            .output()
            .unwrap()
            .status
            .success()
    );
    for web in [true, false] {
        let mut child = command(web)
            .arg("serve")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let mut lines = Vec::new();
        for _ in 0..if web { 3 } else { 2 } {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            lines.push(line.trim().to_owned());
        }
        assert!(
            lines[0].starts_with("JoeDocs MCP endpoint: http://127.0.0.1:"),
            "{lines:?}"
        );
        assert!(
            lines[1].starts_with("JoeDocs admin API: http://127.0.0.1:"),
            "{lines:?}"
        );
        if web {
            assert!(
                lines[2].starts_with("JoeDocs web interface: http://127.0.0.1:"),
                "{lines:?}"
            );
            assert!(lines[2].ends_with("/admin/"), "{lines:?}");
        }
        // Wait until it answers, so the signal handler is in place.
        let addr = lines[0]
            .trim_start_matches("JoeDocs MCP endpoint: http://")
            .trim_end_matches("/mcp")
            .to_owned();
        let mut health = std::net::TcpStream::connect(&addr).unwrap();
        std::io::Write::write_all(
            &mut health,
            format!("GET /health HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .unwrap();
        let mut answer = String::new();
        std::io::Read::read_to_string(&mut health, &mut answer).unwrap();
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        let killed = Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .unwrap();
        assert!(killed.success());
        let mut rest = String::new();
        std::io::Read::read_to_string(&mut reader, &mut rest).unwrap();
        assert!(!rest.contains("web interface"), "{rest}");
        assert!(child.wait().unwrap().success());
    }
}

// --- STEP-06: document library and document view ------------------------------

const PLAN_BODY: &str = "# Rollout\n\n> [!warning] Mind the gap\n> Check [[Second doc]] and [[Missing doc]] first.\n\n| Step | Owner |\n|---|---|\n| One | Ana |\n\n```mermaid\nflowchart LR\n  A --> B\n```\n\n```plantuml\n@startuml\nBob -> Alice : hello\n@enduml\n```\n";

/// A vault with a plan (two revisions), a second document, a multi-file
/// document and an archived one.
fn fixture(store: &mut Store) -> TestResult<()> {
    use joedocs::store::{Actor, NewDocument, NewFile, Update};
    let actor = Actor::new("laptop-key", "claude-code");
    store.add_project("joedocs", "JD", Some("JoeDocs"))?;
    store.add_project("notes", "NT", None)?;
    let fields = |value: serde_json::Value| match value {
        serde_json::Value::Object(map) => map.into_iter().collect(),
        _ => joedocs::document::front_matter::Fields::new(),
    };
    let plan = store.create(
        &actor,
        NewDocument {
            project: "joedocs".into(),
            doc_type: Some("plans".into()),
            title: "Rollout plan".into(),
            content: "First draft.\n".into(),
            front_matter: fields(serde_json::json!({
                "tags": ["release", "web"],
                "related": ["JD-2"],
                "source": "https://example.net/brief",
                "steps_count": 3,
            })),
            ..NewDocument::default()
        },
    )?;
    store.update(
        &actor,
        Update {
            id: plan.id,
            content: PLAN_BODY.into(),
            expected_revision: 1,
            front_matter: joedocs::document::front_matter::Fields::new(),
            summary: Some("Second draft".into()),
        },
    )?;
    store.create(
        &actor,
        NewDocument {
            project: "joedocs".into(),
            doc_type: Some("notes".into()),
            status: Some("approved".into()),
            title: "Second doc".into(),
            content: "No diagrams here.\n".into(),
            ..NewDocument::default()
        },
    )?;
    let backlog = store.create(
        &actor,
        NewDocument {
            project: "joedocs".into(),
            doc_type: Some("notes".into()),
            title: "Backlog".into(),
            content: "The backlog.\n".into(),
            multifile: true,
            ..NewDocument::default()
        },
    )?;
    store.create_file(
        &actor,
        &NewFile {
            document_id: backlog.id,
            file: "items/one".into(),
            content: "Item one body.\n".into(),
            front_matter: fields(serde_json::json!({"state": "planned"})),
            expected_revision: None,
            summary: None,
        },
    )?;
    let old = store.create(
        &actor,
        NewDocument {
            project: "joedocs".into(),
            doc_type: Some("notes".into()),
            title: "Old note".into(),
            content: "Gone.\n".into(),
            ..NewDocument::default()
        },
    )?;
    store.archive(&actor, &old.id, "obsolete")?;
    Ok(())
}

#[tokio::test]
async fn the_library_lists_filters_sorts_and_pages_documents() -> TestResult<()> {
    let server = start_full(|_| {}, fixture).await?;
    let cookie = server.login().await?;
    let all = server
        .get("/admin/documents?project=joedocs", Some(&cookie))
        .await?;
    assert_eq!(all.status, 200, "{}", all.body);
    for expected in [
        "JD-1",
        "Rollout plan",
        "JD-2",
        "Second doc",
        "JD-3",
        "Backlog",
    ] {
        assert!(all.body.contains(expected), "{expected}");
    }
    assert!(
        !all.body.contains("Old note"),
        "archived documents are not listed"
    );
    assert!(all.body.contains("href=\"/admin/documents/JD-1\""));
    // The default project is the first, and the picker lists them all.
    let default = server.get("/admin/documents", Some(&cookie)).await?;
    assert!(default.body.contains("Rollout plan"), "{}", default.body);
    assert!(
        default.body.contains("<option value=\"notes\""),
        "{}",
        default.body
    );

    let approved = server
        .get(
            "/admin/documents?project=joedocs&status=approved",
            Some(&cookie),
        )
        .await?;
    assert!(approved.body.contains("Second doc"));
    assert!(!approved.body.contains("Rollout plan"));
    let notes = server
        .get(
            "/admin/documents?project=joedocs&type=notes&sort=title",
            Some(&cookie),
        )
        .await?;
    let backlog = notes.body.find("Backlog").ok_or("no Backlog")?;
    let second = notes.body.find("Second doc").ok_or("no Second doc")?;
    assert!(backlog < second, "sorted by title");
    assert!(!notes.body.contains("Rollout plan"));

    let page = server
        .get(
            "/admin/documents?project=joedocs&sort=title&limit=1",
            Some(&cookie),
        )
        .await?;
    assert!(page.body.contains("Backlog"));
    assert!(!page.body.contains("Second doc"));
    assert!(
        page.body.contains(
            "href=\"/admin/documents?project=joedocs&#38;sort=title&#38;start=2&#38;limit=1\""
        ),
        "the next page keeps the filters: {}",
        page.body
    );

    let unknown = server
        .get("/admin/documents?project=nope", Some(&cookie))
        .await?;
    assert_eq!(unknown.status, 404);
    assert!(unknown.body.contains("nope"), "{}", unknown.body);
    let bad_sort = server
        .get(
            "/admin/documents?project=joedocs&sort=sideways",
            Some(&cookie),
        )
        .await?;
    assert_eq!(bad_sort.status, 400);
    server.stop().await
}

#[tokio::test]
async fn the_document_view_renders_files_revisions_and_properties() -> TestResult<()> {
    let server = start_full(|_| {}, fixture).await?;
    let cookie = server.login().await?;
    let view = server.get("/admin/documents/JD-1", Some(&cookie)).await?;
    assert_eq!(view.status, 200, "{}", view.body);
    assert!(view.body.contains("callout callout-warning"), "rendered");
    assert!(
        view.body
            .contains("href=\"/admin/documents/JD-2\">Second doc</a>"),
        "wiki link"
    );
    assert!(
        view.body.contains("wikilink unresolved"),
        "missing link marked"
    );
    assert!(view.body.contains("<div class=\"table-wrap\"><table>"));
    // Properties: tags as chips, a document ID linked, a URL linked out.
    assert!(view.body.contains("class=\"properties\""), "{}", view.body);
    assert!(
        view.body.contains("<span class=\"chip\">release</span>"),
        "{}",
        view.body
    );
    assert!(
        view.body
            .contains("href=\"/admin/documents/JD-2\">JD-2</a>"),
        "{}",
        view.body
    );
    assert!(
        view.body
            .contains("href=\"https://example.net/brief\" rel=\"noopener noreferrer\""),
        "{}",
        view.body
    );
    assert!(view.body.contains("steps_count"), "custom keys");
    assert!(view.body.contains("badge-status-draft"), "status badge");
    assert!(
        !view.body.contains("tags:\n"),
        "front matter is not shown as text"
    );
    // Mermaid needs Pele; without a PlantUML server the diagram is source.
    assert!(view.body.contains("/pele.global.js\""), "{}", view.body);
    assert!(!view.body.contains("<img"), "{}", view.body);
    assert!(view.body.contains("PlantUML diagram (source)"));
    // The contents list and the revision picker.
    assert!(view.body.contains("href=\"#h-rollout\""), "on this page");
    assert!(
        view.body
            .contains("href=\"/admin/documents/JD-1?version=1\""),
        "{}",
        view.body
    );
    // Pele sets `style` attributes on its SVG: only a page that loads it
    // allows them, and nothing else is loosened.
    let csp = view.header("content-security-policy").ok_or("no policy")?;
    assert_eq!(csp, format!("{CSP}; style-src-attr 'unsafe-inline'"));
    let second = server.get("/admin/documents/JD-2", Some(&cookie)).await?;
    assert!(
        !second.body.contains("pele.global.js"),
        "no Mermaid, no Pele"
    );
    assert_eq!(second.header("content-security-policy"), Some(CSP));
    server.stop().await
}

#[tokio::test]
async fn the_document_view_shows_the_source_old_revisions_files_and_refusals() -> TestResult<()> {
    let server = start_full(|_| {}, fixture).await?;
    let cookie = server.login().await?;
    let source = server
        .get("/admin/documents/JD-1?view=source", Some(&cookie))
        .await?;
    assert!(
        source.body.contains("tags:"),
        "the stored file: {}",
        source.body
    );
    assert!(
        source.body.contains("&#62; [!warning] Mind the gap"),
        "escaped"
    );
    let first = server
        .get("/admin/documents/JD-1?version=1", Some(&cookie))
        .await?;
    assert!(first.body.contains("First draft."));
    assert!(!first.body.contains("Mind the gap"));
    assert!(first.body.contains("revision 1"), "{}", first.body);

    let multi = server.get("/admin/documents/JD-3", Some(&cookie)).await?;
    assert!(
        multi
            .body
            .contains("href=\"/admin/documents/JD-3?file=items%2Fone\""),
        "{}",
        multi.body
    );
    let file = server
        .get("/admin/documents/JD-3?file=items/one", Some(&cookie))
        .await?;
    assert_eq!(file.status, 200, "{}", file.body);
    assert!(file.body.contains("Item one body."));
    assert!(file.body.contains("planned"), "the file's own keys");
    assert!(!file.body.contains("The backlog."));

    let archived = server.get("/admin/documents/JD-4", Some(&cookie)).await?;
    assert_eq!(archived.status, 409, "{}", archived.body);
    assert!(archived.body.contains("archived"), "{}", archived.body);
    assert!(
        archived.body.contains("href=\"/admin/archive"),
        "{}",
        archived.body
    );
    let missing = server.get("/admin/documents/JD-99", Some(&cookie)).await?;
    assert_eq!(missing.status, 404);

    let tools: Vec<String> = server
        .audit()?
        .into_iter()
        .map(|(tool, _, _)| tool)
        .collect();
    assert!(
        tools.iter().any(|t| t == "admin:documents.get"),
        "{tools:?}"
    );
    server.stop().await
}

#[tokio::test]
async fn plantuml_diagrams_come_from_the_configured_server_only() -> TestResult<()> {
    let server = start_full(
        |c| c.admin.web_plantuml_url = "https://plantuml.example.net/plantuml".into(),
        fixture,
    )
    .await?;
    let cookie = server.login().await?;
    let view = server.get("/admin/documents/JD-1", Some(&cookie)).await?;
    assert!(
        view.body
            .contains("<img src=\"https://plantuml.example.net/plantuml/svg/"),
        "{}",
        view.body
    );
    let csp = view.header("content-security-policy").ok_or("no policy")?;
    assert!(
        csp.contains("img-src 'self' data: https://plantuml.example.net;"),
        "{csp}"
    );
    let list = server
        .get("/admin/documents?project=joedocs", Some(&cookie))
        .await?;
    let tools: Vec<String> = server
        .audit()?
        .into_iter()
        .map(|(tool, _, _)| tool)
        .collect();
    assert!(
        tools.iter().any(|t| t == "admin:documents.list"),
        "{tools:?}"
    );
    assert_eq!(list.status, 200);
    server.stop().await
}
