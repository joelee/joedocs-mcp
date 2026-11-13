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
    /// The body as sent, for downloads.
    bytes: Vec<u8>,
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

/// A `Transfer-Encoding: chunked` body without its chunk framing.
fn dechunk(mut raw: &[u8]) -> TestResult<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let line_end = raw
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("no chunk size")?;
        let size_text = std::str::from_utf8(&raw[..line_end])?;
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)?;
        raw = &raw[line_end + 2..];
        if size == 0 {
            return Ok(body);
        }
        body.extend_from_slice(raw.get(..size).ok_or("short chunk")?);
        raw = raw.get(size + 2..).ok_or("no chunk end")?;
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
        let form_type = if headers.to_ascii_lowercase().contains("content-type:") {
            ""
        } else {
            "content-type: application/x-www-form-urlencoded\r\n"
        };
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n{headers}{form_type}content-length: {}\r\n\r\n{body}",
            body.len()
        );
        let mut stream = TcpStream::connect(self.addr).await?;
        stream.write_all(request.as_bytes()).await?;
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await?;
        let end = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or("no header end")?;
        let head = String::from_utf8_lossy(&raw[..end]).into_owned();
        let mut bytes = raw[end + 4..].to_vec();
        if head
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked")
        {
            bytes = dechunk(&bytes)?;
        }
        let status = head.split(' ').nth(1).ok_or("no status")?.parse()?;
        Ok(Reply {
            status,
            head,
            body: String::from_utf8_lossy(&bytes).into_owned(),
            bytes,
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
    server.stop().await?;

    // With the admin API off as well, the pages still answer as an unknown
    // path does, and only the admin API's own paths answer 404.
    let server = start_with(|c| {
        c.admin.enabled = false;
        c.admin.web_ui = false;
    })
    .await?;
    let unknown = server.get("/no-such-path", None).await?;
    assert_eq!(unknown.status, 401);
    for path in ["/admin/", "/admin/login"] {
        let reply = server.get(path, None).await?;
        assert_eq!(reply.status, unknown.status, "{path}");
        assert_eq!(reply.body, unknown.body, "{path}");
    }
    assert_eq!(server.get("/admin/v1/status", None).await?.status, 404);
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

// --- STEP-07: search -------------------------------------------------------------

/// The STEP-06 vault, plus a note in `notes` whose body needs escaping.
fn search_fixture(store: &mut Store) -> TestResult<()> {
    use joedocs::store::{Actor, NewDocument};
    fixture(store)?;
    store.create(
        &Actor::new("laptop-key", "claude-code"),
        NewDocument {
            project: "notes".into(),
            doc_type: Some("notes".into()),
            title: "Cartoon".into(),
            content: "Tom & Jerry <script>alert(1)</script> chase each other.\n".into(),
            ..NewDocument::default()
        },
    )?;
    Ok(())
}

/// The command lines whose results the Search page must match, as
/// `(joedocs search arguments, the same search as a page query)`.
const SAME_AS_THE_CLI: [(&[&str], &str); 7] = [
    (&["gap OR body"], "q=gap+OR+body"),
    (
        &["-p", "joedocs", "-t", "plans"],
        "project=joedocs&type=plans",
    ),
    (&["-s", "approved"], "status=approved"),
    (&["-F", "state=planned"], "filter=state%3Dplanned"),
    (
        &["--filters", "{\"steps_count\": {\"gt\": 2}}"],
        "filters=%7B%22steps_count%22%3A+%7B%22gt%22%3A+2%7D%7D",
    ),
    (&["-p", "notes", "chase"], "project=notes&q=chase"),
    (&["-p", "joedocs", "chase"], "project=joedocs&q=chase"),
];

/// `ID` or `ID?file=FILE` for each hit of `joedocs search args`.
fn cli_hits(store: &Store, args: &[&str]) -> TestResult<Vec<String>> {
    use clap::Parser as _;
    let mut all = vec!["joedocs", "search"];
    all.extend_from_slice(args);
    let joedocs::Command::Search(args) = joedocs::Cli::try_parse_from(all)?.command else {
        return Err("not a search".into());
    };
    let spec = joedocs::browse::SearchSpec::from_args(&args)?;
    let reply = joedocs::browse::search(store, &spec)?;
    let items = reply["items"].as_array().ok_or("no items")?;
    Ok(items
        .iter()
        .map(|hit| match hit["file"].as_str() {
            Some(file) => format!("{}?file={}", hit["id"].as_str().unwrap_or(""), file),
            None => hit["id"].as_str().unwrap_or("").to_owned(),
        })
        .collect())
}

/// `ID` or `ID?file=FILE` for each hit linked from a results page, in order.
fn page_hits(body: &str) -> Vec<String> {
    let mut hits: Vec<String> = Vec::new();
    for part in body
        .split("<a class=\"hit-link\" href=\"/admin/documents/")
        .skip(1)
    {
        let href = part.split('"').next().unwrap_or("");
        let hit = href.replace("%2F", "/");
        if !hits.contains(&hit) {
            hits.push(hit);
        }
    }
    hits
}

#[tokio::test]
async fn search_matches_the_command_line_and_links_each_file() -> TestResult<()> {
    let expected = Arc::new(std::sync::Mutex::new(Vec::new()));
    let record = Arc::clone(&expected);
    let server = start_full(
        |_| {},
        move |store| {
            search_fixture(store)?;
            let mut out = record.lock().map_err(|_| "poisoned")?;
            for (args, _) in SAME_AS_THE_CLI {
                out.push(cli_hits(store, args)?);
            }
            Ok(())
        },
    )
    .await?;
    let cookie = server.login().await?;
    let expected = expected.lock().map_err(|_| "poisoned")?.clone();
    for ((args, query), want) in SAME_AS_THE_CLI.iter().zip(&expected) {
        let page = server
            .get(&format!("/admin/search?{query}"), Some(&cookie))
            .await?;
        assert_eq!(page.status, 200, "{query}: {}", page.body);
        assert_eq!(&page_hits(&page.body), want, "{args:?} / {query}");
    }
    // The searches above cover a main file, a sub-file, every filter and an
    // empty result; check the fixture really gives them.
    assert_eq!(expected[0], ["JD-3?file=items/one", "JD-1"]);
    assert_eq!(expected[1], ["JD-1"]);
    assert_eq!(expected[2], ["JD-2"]);
    assert_eq!(expected[3], ["JD-3?file=items/one"]);
    assert_eq!(expected[4], ["JD-1"]);
    assert_eq!(expected[5], ["NT-1"]);
    assert_eq!(expected[6], Vec::<String>::new());

    let both = server
        .get("/admin/search?q=gap+OR+body", Some(&cookie))
        .await?;
    assert!(
        both.body
            .contains("<a class=\"hit-link\" href=\"/admin/documents/JD-3?file=items%2Fone\">"),
        "a sub-file hit opens that file: {}",
        both.body
    );
    assert!(both.body.contains("items/one"), "the file is named");
    assert!(
        both.body.contains("value=\"gap OR body\""),
        "the query stays in the form"
    );
    let tools: Vec<String> = server.audit()?.into_iter().map(|(t, _, _)| t).collect();
    assert!(
        tools.iter().any(|t| t == "admin:documents.search"),
        "{tools:?}"
    );
    server.stop().await
}

#[tokio::test]
async fn search_snippets_mark_matches_and_escape_the_rest() -> TestResult<()> {
    let server = start_full(|_| {}, search_fixture).await?;
    let cookie = server.login().await?;
    let page = server.get("/admin/search?q=chase", Some(&cookie)).await?;
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(page.body.contains("<mark>chase</mark>"), "{}", page.body);
    assert!(page.body.contains("Tom &#38; Jerry"), "{}", page.body);
    assert!(
        page.body
            .contains("&#60;script&#62;alert(1)&#60;/script&#62;")
    );
    assert!(!page.body.contains("<script>alert"), "never raw");
    assert!(!page.body.contains('«'), "the markers are replaced");
    server.stop().await
}

#[tokio::test]
async fn search_shows_the_store_message_pages_and_answers_htmx_with_a_fragment() -> TestResult<()> {
    let server = start_full(|_| {}, search_fixture).await?;
    let cookie = server.login().await?;
    for (query, message) in [
        ("q=-gap", "needs at least one word to find"),
        (
            "filters=%7B%22steps_count%22%3A%7B%22bogus%22%3A1%7D%7D",
            "bogus",
        ),
        ("filters=%5B1%5D", "must be a JSON object"),
        ("filter=nokey", "KEY=VALUE"),
        ("type=nope", "nope"),
    ] {
        let page = server
            .get(&format!("/admin/search?{query}"), Some(&cookie))
            .await?;
        assert_eq!(page.status, 400, "{query}: {}", page.body);
        assert!(page.body.contains(message), "{query}: {}", page.body);
        assert!(
            page.body.contains("<form"),
            "the form stays usable: {query}"
        );
    }

    let first = server
        .get("/admin/search?q=gap+OR+body&limit=1", Some(&cookie))
        .await?;
    assert_eq!(page_hits(&first.body), ["JD-3?file=items/one"]);
    assert!(
        first
            .body
            .contains("href=\"/admin/search?q=gap+OR+body&#38;start=2&#38;limit=1\""),
        "the next page keeps the query: {}",
        first.body
    );
    let second = server
        .get("/admin/search?q=gap+OR+body&start=2&limit=1", Some(&cookie))
        .await?;
    assert_eq!(page_hits(&second.body), ["JD-1"]);
    assert!(
        second
            .body
            .contains("href=\"/admin/search?q=gap+OR+body&#38;limit=1\""),
        "the previous page: {}",
        second.body
    );

    // Search as you type: the form reloads only the results region.
    let empty = server.get("/admin/search", Some(&cookie)).await?;
    assert_eq!(empty.status, 200);
    assert_eq!(page_hits(&empty.body), Vec::<String>::new());
    for needle in [
        "hx-get=\"/admin/search\"",
        "hx-trigger=\"input changed delay:300ms, submit\"",
        "hx-target=\"#results\"",
        "hx-push-url=\"true\"",
        "id=\"results\"",
    ] {
        assert!(empty.body.contains(needle), "{needle}: {}", empty.body);
    }
    let headers =
        |kind: &str| format!("Cookie: {cookie}\r\nHX-Request: true\r\nHX-Request-Type: {kind}\r\n");
    let fragment = server
        .send("GET", "/admin/search?q=chase", &headers("partial"), "")
        .await?;
    assert_eq!(fragment.status, 200);
    assert_eq!(page_hits(&fragment.body), ["NT-1"]);
    assert!(!fragment.body.contains("<html"), "{}", fragment.body);
    assert!(!fragment.body.contains("skip-link"), "no layout");
    let refused = server
        .send("GET", "/admin/search?q=-gap", &headers("partial"), "")
        .await?;
    assert_eq!(refused.status, 400);
    assert!(refused.body.contains("needs at least one word"));
    assert!(
        !refused.body.contains("<html"),
        "an error is a fragment too"
    );
    // A history restore asks for the whole page.
    let restore = server
        .send("GET", "/admin/search?q=chase", &headers("full"), "")
        .await?;
    assert!(restore.body.contains("<html"), "{}", restore.body);
    server.stop().await
}

// --- STEP-08: history, diff and downloads -----------------------------------------

/// The STEP-06 vault, with the backlog's file edited (with text that needs
/// escaping) and then renamed: JD-3 revisions 1 create, 2 add `items/one`,
/// 3 edit it, 4 rename it to `items/first`.
fn history_fixture(store: &mut Store) -> TestResult<()> {
    use joedocs::store::Actor;
    fixture(store)?;
    let actor = Actor::new("laptop-key", "claude-code");
    store.update_file(
        &actor,
        "JD-3",
        "items/one",
        "Item one body.\nNow with <b>bold</b> & more.\n",
        &joedocs::document::front_matter::Fields::new(),
        None,
        Some("Edit item one"),
    )?;
    store.rename_file(&actor, "JD-3", "items/one", "items/first", None)?;
    Ok(())
}

/// What the command line gives on the same store, for comparison.
#[derive(Default, Clone)]
struct Cli {
    /// Revision numbers of each history in [`HISTORIES`].
    histories: Vec<Vec<u64>>,
    /// `(added, removed)` lines of each diff in [`DIFFS`].
    diffs: Vec<(Vec<String>, Vec<String>)>,
    /// `joedocs get -f` of JD-1, of JD-1 at revision 1 and of
    /// `items/first`; then `get --archive` of JD-3.
    files: Vec<Vec<u8>>,
}

const HISTORIES: [(&[&str], &str); 4] = [
    (&["JD-1"], "/admin/documents/JD-1/history"),
    (&["JD-3"], "/admin/documents/JD-3/history"),
    (
        &["JD-3", "-P", "items/first"],
        "/admin/documents/JD-3/history?file=items%2Ffirst",
    ),
    (
        &["JD-3", "--start", "2", "-n", "1"],
        "/admin/documents/JD-3/history?start=2&limit=1",
    ),
];

const DIFFS: [(&[&str], &str); 3] = [
    (
        &["JD-1", "--from", "1"],
        "/admin/documents/JD-1/diff?from=1",
    ),
    (
        &["JD-3", "--from", "2", "--to", "4", "-P", "items/first"],
        "/admin/documents/JD-3/diff?from=2&to=4&file=items%2Ffirst",
    ),
    (
        &["JD-3", "--from", "1", "--to", "3"],
        "/admin/documents/JD-3/diff?from=1&to=3",
    ),
];

fn cli_json(store: &Store, args: &[&str]) -> TestResult<serde_json::Value> {
    use clap::Parser as _;
    let all: Vec<&str> = std::iter::once("joedocs")
        .chain(args.iter().copied())
        .collect();
    Ok(match joedocs::Cli::try_parse_from(all)?.command {
        joedocs::Command::History(args) => joedocs::browse::history(store, &args)?,
        joedocs::Command::Diff(args) => joedocs::browse::diff(store, &args)?,
        _ => return Err("not history or diff".into()),
    })
}

fn record_cli(store: &mut Store, out: &std::sync::Mutex<Cli>) -> TestResult<()> {
    history_fixture(store)?;
    let mut cli = out.lock().map_err(|_| "poisoned")?;
    for (args, _) in HISTORIES {
        let args: Vec<&str> = std::iter::once("history")
            .chain(args.iter().copied())
            .collect();
        let reply = cli_json(store, &args)?;
        let revisions = reply["revisions"].as_array().ok_or("no revisions")?;
        cli.histories.push(
            revisions
                .iter()
                .map(|r| r["revision"].as_u64().unwrap_or(0))
                .collect(),
        );
    }
    for (args, _) in DIFFS {
        let args: Vec<&str> = std::iter::once("diff")
            .chain(args.iter().copied())
            .collect();
        let reply = cli_json(store, &args)?;
        let diff = reply["diff"].as_str().ok_or("no diff")?;
        let lines = |sign: char, header: &str| {
            diff.lines()
                .filter(|l| l.starts_with(sign) && !l.starts_with(header))
                .map(|l| l[1..].to_owned())
                .collect::<Vec<_>>()
        };
        cli.diffs.push((lines('+', "+++"), lines('-', "---")));
    }
    cli.files
        .push(joedocs::admin::fetch(store, "JD-1", None)?.into_bytes());
    cli.files
        .push(joedocs::admin::fetch(store, "JD-1", Some(1))?.into_bytes());
    cli.files
        .push(joedocs::admin::fetch_file(store, "JD-3", "items/first", None)?.into_bytes());
    cli.files
        .push(joedocs::admin::folder_archive(store, "JD-3", None)?);
    Ok(())
}

async fn start_history() -> TestResult<(Server, Cli)> {
    let cli = Arc::new(std::sync::Mutex::new(Cli::default()));
    let record = Arc::clone(&cli);
    let server = start_full(|_| {}, move |store| record_cli(store, &record)).await?;
    let cli = cli.lock().map_err(|_| "poisoned")?.clone();
    Ok((server, cli))
}

/// The revision numbers of a history page's rows, in order.
fn rows(body: &str) -> Vec<u64> {
    body.split("<tr data-revision=\"")
        .skip(1)
        .filter_map(|part| part.split('"').next()?.parse().ok())
        .collect()
}

/// `text` as Askama escapes it.
fn escaped(text: &str) -> String {
    text.replace('&', "&#38;")
        .replace('<', "&#60;")
        .replace('>', "&#62;")
        .replace('"', "&#34;")
        .replace('\'', "&#39;")
}

#[tokio::test]
async fn history_lists_revisions_as_the_command_line_does() -> TestResult<()> {
    let (server, cli) = start_history().await?;
    let cookie = server.login().await?;
    for ((args, path), want) in HISTORIES.iter().zip(&cli.histories) {
        let page = server.get(path, Some(&cookie)).await?;
        assert_eq!(page.status, 200, "{path}: {}", page.body);
        assert_eq!(&rows(&page.body), want, "{args:?} / {path}");
    }
    assert_eq!(cli.histories[1], [4, 3, 2, 1], "newest first");
    assert_eq!(cli.histories[2], [4, 3, 2], "the file across its rename");
    assert_eq!(cli.histories[3], [3]);

    let page = server
        .get("/admin/documents/JD-3/history", Some(&cookie))
        .await?;
    for expected in [
        "Edit item one",
        "laptop-key",
        "claude-code",
        "<code>rename</code>",
        // Each row opens that revision and its diff with the one before.
        "href=\"/admin/documents/JD-3?version=3\"",
        "href=\"/admin/documents/JD-3/diff?from=2&#38;to=3\"",
        "href=\"/admin/documents/JD-3\"",
    ] {
        assert!(page.body.contains(expected), "{expected}: {}", page.body);
    }
    assert!(
        !page.body.contains("diff?from=0"),
        "the first revision has nothing before it"
    );
    let file = server
        .get(
            "/admin/documents/JD-3/history?file=items%2Ffirst",
            Some(&cookie),
        )
        .await?;
    assert!(
        file.body
            .contains("href=\"/admin/documents/JD-3?file=items%2Fone&#38;version=3\""),
        "a row opens the file by its name at that revision: {}",
        file.body
    );
    assert!(
        file.body
            .contains("href=\"/admin/documents/JD-3/diff?from=3&#38;to=4&#38;file=items%2Ffirst\""),
        "{}",
        file.body
    );
    let first = server
        .get("/admin/documents/JD-3/history?limit=1", Some(&cookie))
        .await?;
    assert_eq!(rows(&first.body), [4]);
    assert!(
        first
            .body
            .contains("href=\"/admin/documents/JD-3/history?start=2&#38;limit=1\""),
        "{}",
        first.body
    );
    let missing = server
        .get("/admin/documents/JD-3/history?file=nope", Some(&cookie))
        .await?;
    assert_eq!(missing.status, 404, "{}", missing.body);
    let tools: Vec<String> = server.audit()?.into_iter().map(|(t, _, _)| t).collect();
    assert!(
        tools.iter().any(|t| t == "admin:documents.history"),
        "{tools:?}"
    );
    server.stop().await
}

#[tokio::test]
async fn diffs_mark_the_lines_the_command_line_shows() -> TestResult<()> {
    let (server, cli) = start_history().await?;
    let cookie = server.login().await?;
    for ((args, path), (added, removed)) in DIFFS.iter().zip(&cli.diffs) {
        let page = server.get(path, Some(&cookie)).await?;
        assert_eq!(page.status, 200, "{path}: {}", page.body);
        assert!(!added.is_empty() || !removed.is_empty(), "{args:?}");
        for line in added {
            let html = format!(
                "<span class=\"diff-line diff-add\">+{}</span>",
                escaped(line)
            );
            assert!(page.body.contains(&html), "{args:?}: {html}\n{}", page.body);
        }
        for line in removed {
            let html = format!(
                "<span class=\"diff-line diff-del\">-{}</span>",
                escaped(line)
            );
            assert!(page.body.contains(&html), "{args:?}: {html}\n{}", page.body);
        }
        assert!(page.body.contains("diff-hunk"), "{path}");
        assert!(!page.body.contains("<b>bold</b>"), "never raw: {path}");
    }
    assert!(
        cli.diffs[1]
            .0
            .iter()
            .any(|l| l.contains("<b>bold</b> & more")),
        "the fixture has text to escape: {:?}",
        cli.diffs[1]
    );
    let same = server
        .get("/admin/documents/JD-1/diff?from=2&to=2", Some(&cookie))
        .await?;
    assert_eq!(same.status, 200);
    assert!(same.body.contains("No changes"), "{}", same.body);
    let missing = server
        .get("/admin/documents/JD-1/diff?from=99", Some(&cookie))
        .await?;
    assert_eq!(missing.status, 400, "{}", missing.body);
    assert!(
        missing.body.contains("JD-1 has revisions 1 to 2"),
        "the store's message: {}",
        missing.body
    );
    let no_from = server
        .get("/admin/documents/JD-1/diff", Some(&cookie))
        .await?;
    assert_eq!(no_from.status, 400, "{}", no_from.body);
    let tools: Vec<String> = server.audit()?.into_iter().map(|(t, _, _)| t).collect();
    assert!(
        tools.iter().any(|t| t == "admin:documents.diff"),
        "{tools:?}"
    );
    server.stop().await
}

#[tokio::test]
async fn downloads_hold_the_bytes_the_command_line_writes() -> TestResult<()> {
    let (server, cli) = start_history().await?;
    let cookie = server.login().await?;
    for (path, name, bytes) in [
        ("/admin/documents/JD-1/download", "JD-1.md", &cli.files[0]),
        (
            "/admin/documents/JD-1/download?version=1",
            "JD-1-r1.md",
            &cli.files[1],
        ),
        (
            "/admin/documents/JD-3/download?file=items%2Ffirst",
            "JD-3-items-first.md",
            &cli.files[2],
        ),
    ] {
        let reply = server.get(path, Some(&cookie)).await?;
        assert_eq!(reply.status, 200, "{path}: {}", reply.body);
        assert_eq!(
            reply.header("content-disposition"),
            Some(format!("attachment; filename=\"{name}\"").as_str()),
            "{path}"
        );
        assert_eq!(
            reply.header("content-type"),
            Some("text/markdown; charset=utf-8"),
            "{path}"
        );
        assert_eq!(&reply.bytes, bytes, "{path}");
    }
    let archive = server
        .get("/admin/documents/JD-3/download?archive=true", Some(&cookie))
        .await?;
    assert_eq!(archive.status, 200, "{}", archive.body);
    assert_eq!(archive.header("content-type"), Some("application/gzip"));
    assert_eq!(
        archive.header("content-disposition"),
        Some("attachment; filename=\"JD-3.tgz\"")
    );
    joedocs::admin::check_folder_archive(&archive.bytes)?;
    assert_eq!(archive.bytes, cli.files[3]);
    let single = server
        .get("/admin/documents/JD-1/download?archive=true", Some(&cookie))
        .await?;
    assert_eq!(single.status, 400);
    assert!(
        single.body.contains("single-file document"),
        "{}",
        single.body
    );

    // The document view offers the downloads and the History tab.
    let view = server.get("/admin/documents/JD-3", Some(&cookie)).await?;
    for link in [
        "href=\"/admin/documents/JD-3/download\"",
        "href=\"/admin/documents/JD-3/download?archive=true\"",
        "href=\"/admin/documents/JD-3/history\"",
    ] {
        assert!(view.body.contains(link), "{link}: {}", view.body);
    }
    let old = server
        .get(
            "/admin/documents/JD-3?file=items%2Ffirst&version=3",
            Some(&cookie),
        )
        .await?;
    assert_eq!(
        old.status, 404,
        "items/first did not exist yet at revision 3"
    );
    let old = server
        .get(
            "/admin/documents/JD-3?file=items%2Fone&version=3",
            Some(&cookie),
        )
        .await?;
    assert!(
        old.body
            .contains("href=\"/admin/documents/JD-3/download?file=items%2Fone&#38;version=3\""),
        "{}",
        old.body
    );
    assert!(
        old.body
            .contains("href=\"/admin/documents/JD-3/history?file=items%2Fone\""),
        "{}",
        old.body
    );
    server.stop().await
}

// --- STEP-09: projects and API keys ------------------------------------------------

impl Server {
    /// Posts `fields` (and the CSRF token, when given) from this server's
    /// own origin with the session `cookie`.
    async fn post_form(
        &self,
        path: &str,
        cookie: &str,
        csrf: Option<&str>,
        fields: &[(&str, &str)],
    ) -> TestResult<Reply> {
        let mut pairs: Vec<(&str, &str)> = fields.to_vec();
        if let Some(csrf) = csrf {
            pairs.push(("csrf", csrf));
        }
        let headers = format!(
            "Cookie: {cookie}\r\nOrigin: {}\r\nSec-Fetch-Site: same-origin\r\n",
            self.origin()
        );
        self.send("POST", path, &headers, &form(&pairs)).await
    }

    /// The status `/mcp` gives an `initialize` with `token`.
    async fn mcp_status(&self, token: &str) -> TestResult<u16> {
        let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"raw","version":"1"}}}"#;
        let headers = format!(
            "Authorization: Bearer {token}\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\n"
        );
        Ok(self.send("POST", "/mcp", &headers, init).await?.status)
    }

    /// Stops the server and keeps its directory, for the command line.
    async fn stop_keeping(self) -> TestResult<TempDir> {
        let Self {
            dir,
            mut stop,
            mut task,
            ..
        } = self;
        if let Some(stop) = stop.take() {
            stop.send(()).ok();
        }
        (&mut task).await??;
        Ok(dir)
    }
}

/// What `joedocs <args>` prints on the store in `dir`.
fn joedocs_cli(dir: &std::path::Path, args: &[&str]) -> TestResult<String> {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_joedocs"));
    cmd.env_clear()
        .env("HOME", dir)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("JOEDOCS__STORAGE__HOME", dir.join("vault"))
        .env("JOEDOCS__STORAGE__STATE_DIR", dir.join("state"))
        .current_dir(dir)
        .args(args);
    if let Ok(profile) = std::env::var("LLVM_PROFILE_FILE") {
        cmd.env("LLVM_PROFILE_FILE", profile);
    }
    let output = cmd.output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

/// The table row marked `data-<what>="<name>"`.
fn row_of<'a>(body: &'a str, what: &str, name: &str) -> &'a str {
    let marker = format!("<tr data-{what}=\"{name}\"");
    body.find(&marker).map_or("", |start| {
        let rest = &body[start..];
        &rest[..rest.find("</tr>").unwrap_or(rest.len())]
    })
}

#[tokio::test]
async fn projects_are_listed_and_added_with_the_rules_of_project_add() -> TestResult<()> {
    let server = start_full(|_| {}, fixture).await?;
    let cookie = server.login().await?;
    let csrf = server.csrf(&cookie).await?;
    let page = server.get("/admin/projects", Some(&cookie)).await?;
    assert_eq!(page.status, 200, "{}", page.body);
    let joedocs = row_of(&page.body, "project", "joedocs");
    for expected in ["JD", "JoeDocs", "<td class=\"num\">3</td>", ">1</a>"] {
        assert!(joedocs.contains(expected), "{expected}: {joedocs}");
    }
    assert!(row_of(&page.body, "project", "notes").contains("NT"));

    let added = server
        .post_form(
            "/admin/projects",
            &cookie,
            Some(&csrf),
            &[("name", "alpha"), ("key", "AL"), ("title", "Alpha docs")],
        )
        .await?;
    assert_eq!(added.status, 303, "{}", added.body);
    let location = added.header("location").ok_or("no location")?.to_owned();
    assert_eq!(location, "/admin/projects?added=alpha");
    let after = server.get(&location, Some(&cookie)).await?;
    assert!(
        after
            .body
            .contains("Added project alpha (key AL): Alpha docs"),
        "the message `project add` prints: {}",
        after.body
    );
    for (fields, status, message) in [
        (
            [("name", "Bad Name"), ("key", "BN"), ("title", "")],
            400,
            "must be lower-case letters, digits and &#39;-&#39;, starting with a letter or digit",
        ),
        (
            [("name", "beta"), ("key", "be"), ("title", "")],
            400,
            "must be 1 to 10 upper-case letters and digits, starting with a letter",
        ),
        (
            [("name", "joedocs"), ("key", "ZZ"), ("title", "")],
            409,
            "project joedocs already exists",
        ),
        (
            [("name", "beta"), ("key", "JD"), ("title", "")],
            409,
            "project key JD (used by joedocs) already exists",
        ),
    ] {
        let refused = server
            .post_form("/admin/projects", &cookie, Some(&csrf), &fields)
            .await?;
        assert_eq!(refused.status, status, "{fields:?}: {}", refused.body);
        assert!(
            refused.body.contains(message),
            "{fields:?}: {}",
            refused.body
        );
        assert!(
            refused
                .body
                .contains(&format!("value=\"{}\"", escaped(fields[0].1))),
            "the form keeps what was typed: {}",
            refused.body
        );
    }
    let no_csrf = server
        .post_form(
            "/admin/projects",
            &cookie,
            None,
            &[("name", "gamma"), ("key", "GA")],
        )
        .await?;
    assert_eq!(no_csrf.status, 403);
    let tools: Vec<String> = server.audit()?.into_iter().map(|(t, _, _)| t).collect();
    for tool in ["admin:projects.list", "admin:projects.add"] {
        assert!(tools.iter().any(|t| t == tool), "{tool}: {tools:?}");
    }

    let dir = server.stop_keeping().await?;
    let listed = joedocs_cli(dir.path(), &["project", "list"])?;
    let alpha = listed
        .lines()
        .find(|l| l.starts_with("alpha "))
        .ok_or("alpha is not listed")?;
    assert!(
        alpha.contains("AL") && alpha.contains("Alpha docs"),
        "{listed}"
    );
    assert!(
        !listed.contains("beta") && !listed.contains("gamma"),
        "{listed}"
    );
    Ok(())
}

/// Every log line the process writes from now on, as text.
fn capture_logs() -> Arc<std::sync::Mutex<Vec<u8>>> {
    #[derive(Clone)]
    struct Sink(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if let Ok(mut out) = self.0.lock() {
                out.extend_from_slice(buf);
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buffer = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Sink(Arc::clone(&buffer));
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || sink.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber).ok();
    buffer
}

/// The token on a reveal-once page.
fn revealed(body: &str) -> TestResult<String> {
    let marker = "<code id=\"new-token\">";
    let start = body.find(marker).ok_or("no token shown")? + marker.len();
    let end = body[start..].find('<').ok_or("unterminated token")?;
    Ok(body[start..start + end].to_owned())
}

/// Creates key `ci` with write on joedocs and read on notes, and returns
/// the reply that shows its token.
async fn create_ci_key(server: &Server, cookie: &str, csrf: &str) -> TestResult<Reply> {
    server
        .post_form(
            "/admin/keys",
            cookie,
            Some(csrf),
            &[
                ("name", "ci"),
                ("grant.joedocs", "write"),
                ("grant.notes", "read"),
            ],
        )
        .await
}

/// `joedocs key list` on the store in `dir`, and the line of key `name`.
fn key_list_line(dir: &std::path::Path, name: &str) -> TestResult<(String, String)> {
    let keys = joedocs_cli(dir, &["key", "list"])?;
    let line = keys
        .lines()
        .find(|l| l.starts_with(&format!("{name} ")))
        .ok_or("the key is not listed")?
        .to_owned();
    Ok((keys, line))
}

/// Each key action without its CSRF token is refused.
async fn every_key_action_needs_its_csrf_token(server: &Server, cookie: &str) -> TestResult<()> {
    for (action, fields) in [
        ("/admin/keys", vec![("name", "other")]),
        (
            "/admin/keys/ci/grant",
            vec![("project", "notes"), ("permission", "read")],
        ),
        ("/admin/keys/ci/ungrant", vec![("project", "notes")]),
        ("/admin/keys/ci/rotate", vec![]),
        ("/admin/keys/ci/revoke", vec![]),
    ] {
        let refused = server.post_form(action, cookie, None, &fields).await?;
        assert_eq!(refused.status, 403, "{action} without its CSRF token");
    }
    Ok(())
}

#[tokio::test]
async fn a_new_key_is_shown_once_and_its_grants_change_as_the_key_commands_do() -> TestResult<()> {
    let server = start_full(|_| {}, fixture).await?;
    let cookie = server.login().await?;
    let csrf = server.csrf(&cookie).await?;
    let empty = server.get("/admin/keys", Some(&cookie)).await?;
    assert_eq!(empty.status, 200, "{}", empty.body);

    let created = create_ci_key(&server, &cookie, &csrf).await?;
    assert_eq!(
        created.status, 200,
        "the token is on this reply: {}",
        created.body
    );
    assert_eq!(created.header("location"), None, "no redirect");
    assert_eq!(created.header("cache-control"), Some("no-store"));
    let token = revealed(&created.body)?;
    assert!(token.starts_with("jd_"), "{token}");
    assert!(
        created.body.contains("joedocs:write, notes:read"),
        "{}",
        created.body
    );
    assert_eq!(server.mcp_status(&token).await?, 200, "the new token works");

    let listed = server.get("/admin/keys", Some(&cookie)).await?;
    let ci = row_of(&listed.body, "key", "ci");
    assert!(
        ci.contains("joedocs:write") && ci.contains("notes:read"),
        "{ci}"
    );
    for needle in [
        "<dialog id=\"rotate-ci\"",
        "Rotate API key ci?",
        "Revoke API key ci?",
    ] {
        assert!(listed.body.contains(needle), "{needle}: {}", listed.body);
    }

    let granted = server
        .post_form(
            "/admin/keys/ci/grant",
            &cookie,
            Some(&csrf),
            &[("project", "notes"), ("permission", "write")],
        )
        .await?;
    assert_eq!(granted.status, 303, "{}", granted.body);
    let ungranted = server
        .post_form(
            "/admin/keys/ci/ungrant",
            &cookie,
            Some(&csrf),
            &[("project", "joedocs")],
        )
        .await?;
    assert_eq!(ungranted.status, 303, "{}", ungranted.body);
    let after = server
        .get(
            ungranted.header("location").ok_or("no location")?,
            Some(&cookie),
        )
        .await?;
    assert!(
        after.body.contains("API key ci grants: notes:write"),
        "{}",
        after.body
    );
    let nothing = server
        .post_form(
            "/admin/keys/ci/ungrant",
            &cookie,
            Some(&csrf),
            &[("project", "joedocs")],
        )
        .await?;
    assert_eq!(nothing.status, 400, "{}", nothing.body);
    assert!(
        nothing.body.contains("API key ci has no grant on joedocs"),
        "{}",
        nothing.body
    );
    assert!(
        row_of(&nothing.body, "key", "ci").contains("notes:write"),
        "unchanged"
    );

    every_key_action_needs_its_csrf_token(&server, &cookie).await?;
    assert_eq!(
        server.mcp_status(&token).await?,
        200,
        "nothing rotated or revoked"
    );

    let dir = server.stop_keeping().await?;
    let (keys, line) = key_list_line(dir.path(), "ci")?;
    assert!(
        line.contains("active") && line.contains("notes:write"),
        "{keys}"
    );
    assert!(!line.contains("joedocs:"), "{keys}");
    assert!(!keys.contains("other"), "{keys}");
    Ok(())
}

#[tokio::test]
async fn rotate_and_revoke_stop_old_tokens_and_no_token_is_kept() -> TestResult<()> {
    let logs = capture_logs();
    let server = start_full(|_| {}, fixture).await?;
    let cookie = server.login().await?;
    let csrf = server.csrf(&cookie).await?;
    let token = revealed(&create_ci_key(&server, &cookie, &csrf).await?.body)?;
    let rotated = server
        .post_form("/admin/keys/ci/rotate", &cookie, Some(&csrf), &[])
        .await?;
    assert_eq!(rotated.status, 200, "{}", rotated.body);
    assert_eq!(rotated.header("location"), None, "no redirect");
    let new_token = revealed(&rotated.body)?;
    assert_ne!(new_token, token);
    assert_eq!(
        server.mcp_status(&token).await?,
        401,
        "the old token stops working"
    );
    assert_eq!(server.mcp_status(&new_token).await?, 200);
    let revoked = server
        .post_form("/admin/keys/ci/revoke", &cookie, Some(&csrf), &[])
        .await?;
    assert_eq!(revoked.status, 303, "{}", revoked.body);
    let after = server
        .get(
            revoked.header("location").ok_or("no location")?,
            Some(&cookie),
        )
        .await?;
    assert!(
        after.body.contains("API key ci is revoked (since"),
        "{}",
        after.body
    );
    assert_eq!(
        server.mcp_status(&new_token).await?,
        401,
        "a revoked key stops working"
    );

    // Neither token is shown again, stored, audited or logged.
    for path in ["/admin/keys", "/admin/"] {
        let later = server.get(path, Some(&cookie)).await?;
        assert!(
            !later.body.contains(&token) && !later.body.contains(&new_token),
            "{path}"
        );
    }
    let tools: Vec<String> = server.audit()?.into_iter().map(|(t, _, _)| t).collect();
    for tool in [
        "admin:keys.list",
        "admin:keys.create",
        "admin:keys.rotate",
        "admin:keys.revoke",
    ] {
        assert!(tools.iter().any(|t| t == tool), "{tool}: {tools:?}");
    }
    let dir = server.stop_keeping().await?;
    let secrets = [&token, &new_token].map(|t| t.rsplit('_').next().unwrap_or("").to_owned());
    for name in ["joedocs.db", "joedocs.db-wal"] {
        let bytes = std::fs::read(dir.path().join("state").join(name)).unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes);
        assert!(secrets.iter().all(|s| !text.contains(s.as_str())), "{name}");
    }
    let logged = String::from_utf8_lossy(&logs.lock().map_err(|_| "poisoned")?).into_owned();
    assert!(
        secrets.iter().all(|s| !logged.contains(s.as_str())),
        "a token was logged"
    );

    let (keys, line) = key_list_line(dir.path(), "ci")?;
    assert!(line.contains("revoked"), "{keys}");
    Ok(())
}

// --- STEP-10: audit --------------------------------------------------------------

/// The STEP-06 vault, an API key `ci`, and audit rows from MCP clients that
/// no web request can match: key `ci-bot`, MCP tools.
fn audit_fixture(store: &mut Store) -> TestResult<()> {
    use joedocs::store::Actor;
    fixture(store)?;
    store.create_key("ci", &[], &joedocs::auth::OsRandom)?;
    let bot = Actor::new("ci-bot", "claude-code");
    for (tool, document, outcome) in [
        ("new_document", Some("JD-1"), "ok"),
        ("update_document", Some("JD-1"), "conflict"),
        ("new_document", Some("JD-2"), "ok"),
        ("archive_document", Some("JD-4"), "ok"),
    ] {
        store.audit("bot-id", &bot, tool, document, outcome)?;
    }
    Ok(())
}

/// Audit filters whose results must match `joedocs audit`, as
/// `(command line arguments, page query)`.
const AUDIT_FILTERS: [(&[&str], &str); 8] = [
    (&["-k", "ci-bot"], "key=ci-bot"),
    (
        &["-k", "ci-bot", "--tool", "new_document"],
        "key=ci-bot&tool=new_document",
    ),
    (&["--tool", "new_*"], "tool=new_%2A"),
    (&["-d", "JD-1"], "document=JD-1"),
    (&["--outcome", "conflict"], "outcome=conflict"),
    (
        &["-k", "ci-bot", "--since", "2026-10-26"],
        "key=ci-bot&since=2026-10-26",
    ),
    (
        &["-k", "ci-bot", "--until", "2026-10-26"],
        "key=ci-bot&until=2026-10-26",
    ),
    (
        &["-k", "ci-bot", "--start", "2", "-n", "2"],
        "key=ci-bot&start=2&limit=2",
    ),
];

/// `tool document outcome` for each row of `joedocs audit args`.
fn cli_audit(store: &Store, args: &[&str]) -> TestResult<Vec<String>> {
    use clap::Parser as _;
    let all: Vec<&str> = ["joedocs", "audit"]
        .into_iter()
        .chain(args.iter().copied())
        .collect();
    let joedocs::Command::Audit(args) = joedocs::Cli::try_parse_from(all)?.command else {
        return Err("not audit".into());
    };
    let reply = joedocs::browse::audit(store, &args)?;
    let items = reply["items"].as_array().ok_or("no items")?;
    Ok(items
        .iter()
        .map(|row| {
            format!(
                "{} {} {}",
                row["tool"].as_str().unwrap_or(""),
                row["document_id"].as_str().unwrap_or("-"),
                row["outcome"].as_str().unwrap_or("")
            )
        })
        .collect())
}

/// `tool document outcome` for each row of an audit page.
fn page_entries(body: &str) -> Vec<String> {
    body.split("<tr data-entry=\"")
        .skip(1)
        .filter_map(|part| part.split('"').next().map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn the_audit_page_filters_and_pages_as_joedocs_audit_does() -> TestResult<()> {
    let expected = Arc::new(std::sync::Mutex::new(Vec::new()));
    let record = Arc::clone(&expected);
    let server = start_full(
        |_| {},
        move |store| {
            audit_fixture(store)?;
            let mut out = record.lock().map_err(|_| "poisoned")?;
            for (args, _) in AUDIT_FILTERS {
                out.push(cli_audit(store, args)?);
            }
            Ok(())
        },
    )
    .await?;
    let cookie = server.login().await?;
    let expected = expected.lock().map_err(|_| "poisoned")?.clone();
    for ((args, query), want) in AUDIT_FILTERS.iter().zip(&expected) {
        let page = server
            .get(&format!("/admin/audit?{query}"), Some(&cookie))
            .await?;
        assert_eq!(page.status, 200, "{query}: {}", page.body);
        assert_eq!(&page_entries(&page.body), want, "{args:?} / {query}");
    }
    assert_eq!(expected[0].len(), 4);
    assert_eq!(
        expected[2],
        ["new_document JD-2 ok", "new_document JD-1 ok"]
    );
    assert_eq!(expected[4], ["update_document JD-1 conflict"]);
    assert!(expected[6].is_empty(), "until is exclusive");
    assert_eq!(expected[7].len(), 2);

    let all = server.get("/admin/audit", Some(&cookie)).await?;
    let row = all
        .body
        .split("<tr data-entry=\"update_document JD-1 conflict\"")
        .nth(1)
        .and_then(|rest| rest.split("</tr>").next())
        .ok_or("no conflict row")?;
    for cell in [
        "ci-bot",
        "claude-code",
        "update_document",
        "conflict",
        "2026-10-26T07:33:20Z",
    ] {
        assert!(row.contains(cell), "{cell}: {row}");
    }
    assert!(row.contains("href=\"/admin/documents/JD-1\""), "{row}");
    for quick in [
        "href=\"/admin/audit?tool=admin%3A*\"",
        "href=\"/admin/audit?outcome_not=ok\"",
        "href=\"/admin/audit?since=",
    ] {
        assert!(all.body.contains(quick), "{quick}: {}", all.body);
    }
    let refusals = server
        .get("/admin/audit?outcome_not=ok&key=ci-bot", Some(&cookie))
        .await?;
    assert_eq!(
        page_entries(&refusals.body),
        ["update_document JD-1 conflict"]
    );

    let first = server
        .get("/admin/audit?key=ci-bot&limit=1", Some(&cookie))
        .await?;
    assert!(
        first
            .body
            .contains("href=\"/admin/audit?key=ci-bot&#38;start=2&#38;limit=1\""),
        "paging keeps the filters: {}",
        first.body
    );
    for (query, message) in [
        ("since=yesterday", "is not a UTC date (2026-11-10) or time"),
        ("document=nope", "nope"),
        ("limit=0", "must be 1 to"),
    ] {
        let bad = server
            .get(&format!("/admin/audit?{query}"), Some(&cookie))
            .await?;
        assert_eq!(bad.status, 400, "{query}: {}", bad.body);
        assert!(bad.body.contains(message), "{query}: {}", bad.body);
        assert!(
            bad.body.contains("<form"),
            "the filters stay usable: {query}"
        );
    }
    server.stop().await
}

/// Each route in the table of `src/web/mod.rs`'s documentation, as
/// `(method, path, audit tool)`.
fn documented_routes() -> Vec<(String, String, String)> {
    include_str!("../src/web/mod.rs")
        .lines()
        .filter_map(|line| {
            let row = line.strip_prefix("//! | `")?;
            let (route, rest) = row.split_once("` | `")?;
            let tool = rest.strip_suffix("` |")?;
            let (method, path) = route.split_once(' ')?;
            Some((method.to_owned(), path.to_owned(), tool.to_owned()))
        })
        .collect()
}

#[tokio::test]
async fn every_route_writes_one_audit_row_with_its_documented_tool() -> TestResult<()> {
    let server = start_full(|_| {}, audit_fixture).await?;
    let routes = documented_routes();
    assert!(routes.len() >= 19, "{routes:?}");
    assert!(
        routes
            .iter()
            .any(|(_, p, t)| p == "/admin/audit" && t == "admin:audit.list")
    );
    let mut cookie = server.login().await?;
    let mut csrf = server.csrf(&cookie).await?;
    for (method, path, tool) in &routes {
        let path = path.replace("{id}", "JD-1").replace("{name}", "ci");
        let before = server.audit()?.len();
        let reply = match (method.as_str(), path.as_str()) {
            ("POST", "/admin/login") => server.post_login(&server.code(), "/admin/").await?,
            ("POST", "/admin/logout") => {
                let reply = server.post_form(&path, &cookie, Some(&csrf), &[]).await?;
                server.clock.advance(30);
                cookie = server.login().await?;
                csrf = server.csrf(&cookie).await?;
                reply
            }
            ("POST", _) => server.post_form(&path, &cookie, Some(&csrf), &[]).await?,
            _ => server.get(&path, Some(&cookie)).await?,
        };
        assert!(reply.status < 500, "{method} {path}: {}", reply.body);
        let rows = server.audit()?;
        let written: Vec<_> = rows[before..]
            .iter()
            .filter(|(t, _, _)| t == tool)
            .collect();
        assert_eq!(
            written.len(),
            1,
            "{method} {path} → {tool}: {:?}",
            &rows[before..]
        );
        assert_eq!(written[0].1, "joedocs-web", "{method} {path}");
    }
    let unknown_before = server.audit()?.len();
    server.get("/admin/nowhere", Some(&cookie)).await?;
    let rows = server.audit()?;
    assert_eq!(rows[unknown_before..].len(), 1);
    assert_eq!(rows[unknown_before].0, "admin:web.unknown");
    let page = server.get("/admin/", Some(&cookie)).await?;
    let asset = page
        .body
        .split("href=\"/admin/assets/")
        .nth(1)
        .and_then(|r| r.split('"').next());
    let before = server.audit()?.len();
    let stylesheet = server
        .get(&format!("/admin/assets/{}", asset.ok_or("no asset")?), None)
        .await?;
    assert_eq!(stylesheet.status, 200);
    assert_eq!(
        server.audit()?.len(),
        before,
        "static files are not audited"
    );
    server.stop().await
}

// --- STEP-11: archive and structure actions ----------------------------------------

/// The STEP-08 vault plus a second backlog file: JD-3 has `items/first`
/// (once `items/one`) and `items/second`; JD-4 is archived.
fn structure_fixture(store: &mut Store) -> TestResult<()> {
    use joedocs::store::{Actor, NewFile};
    history_fixture(store)?;
    store.create_file(
        &Actor::new("laptop-key", "claude-code"),
        &NewFile {
            document_id: "JD-3".into(),
            file: "items/second".into(),
            content: "Item two.\n".into(),
            front_matter: joedocs::document::front_matter::Fields::new(),
            expected_revision: None,
            summary: None,
        },
    )?;
    Ok(())
}

/// Whether the newest History row of `path` was written by the signed-in
/// admin token (`laptop`) from the web interface.
async fn latest_by_web(server: &Server, cookie: &str, path: &str) -> TestResult<bool> {
    let history = server.get(path, Some(cookie)).await?;
    let newest = history
        .body
        .split("<tr data-revision=\"")
        .nth(1)
        .and_then(|row| row.split("</tr>").next())
        .unwrap_or("");
    Ok(newest.contains("<td>laptop <span class=\"hint\">joedocs-web</span></td>"))
}

#[tokio::test]
async fn the_archive_lists_and_restores_documents() -> TestResult<()> {
    let server = start_full(|_| {}, structure_fixture).await?;
    let cookie = server.login().await?;
    let csrf = server.csrf(&cookie).await?;
    let archive = server.get("/admin/archive", Some(&cookie)).await?;
    assert_eq!(archive.status, 200, "{}", archive.body);
    let row = row_of(&archive.body, "archived", "JD-4");
    for cell in ["Old note", "joedocs", "notes", "obsolete", "laptop-key"] {
        assert!(row.contains(cell), "{cell}: {row}");
    }
    for (query, listed) in [
        ("project=notes", false),
        ("type=notes", true),
        ("project=joedocs&type=plans", false),
    ] {
        let page = server
            .get(&format!("/admin/archive?{query}"), Some(&cookie))
            .await?;
        assert_eq!(page.status, 200, "{query}");
        assert_eq!(
            page.body.contains("data-archived=\"JD-4\""),
            listed,
            "{query}"
        );
    }
    let bad = server
        .get("/admin/archive?type=nope", Some(&cookie))
        .await?;
    assert_eq!(bad.status, 400);
    assert!(bad.body.contains("nope"), "{}", bad.body);

    let restored = server
        .post_form("/admin/archive/JD-4/restore", &cookie, Some(&csrf), &[])
        .await?;
    assert_eq!(restored.status, 303, "{}", restored.body);
    let location = restored.header("location").ok_or("no location")?.to_owned();
    assert_eq!(location, "/admin/documents/JD-4?done=unarchived");
    let view = server.get(&location, Some(&cookie)).await?;
    assert_eq!(view.status, 200, "{}", view.body);
    assert!(
        view.body
            .contains("Restored JD-4 from the archive; it is now at revision 3."),
        "{}",
        view.body
    );
    assert!(latest_by_web(&server, &cookie, "/admin/documents/JD-4/history").await?);
    let empty = server.get("/admin/archive", Some(&cookie)).await?;
    assert!(!empty.body.contains("data-archived=\"JD-4\""));
    let again = server
        .post_form("/admin/archive/JD-4/restore", &cookie, Some(&csrf), &[])
        .await?;
    assert!(again.status >= 400 && again.status < 500, "{}", again.body);
    assert!(
        again.body.contains("JD-4"),
        "the store's message: {}",
        again.body
    );
    server.stop().await
}

#[tokio::test]
async fn convert_makes_a_document_multi_file_with_the_messages_of_document_convert()
-> TestResult<()> {
    let server = start_full(|_| {}, structure_fixture).await?;
    let cookie = server.login().await?;
    let csrf = server.csrf(&cookie).await?;
    let single = server.get("/admin/documents/JD-2", Some(&cookie)).await?;
    assert!(
        single
            .body
            .contains("action=\"/admin/documents/JD-2/convert\""),
        "{}",
        single.body
    );
    let multi = server.get("/admin/documents/JD-3", Some(&cookie)).await?;
    assert!(
        !multi.body.contains("/convert\""),
        "not offered on a multi-file document"
    );
    let file = server
        .get("/admin/documents/JD-3?file=items%2Fsecond", Some(&cookie))
        .await?;
    assert!(
        file.body
            .contains("action=\"/admin/documents/JD-3/move-file\""),
        "{}",
        file.body
    );

    let converted = server
        .post_form("/admin/documents/JD-2/convert", &cookie, Some(&csrf), &[])
        .await?;
    assert_eq!(converted.status, 303, "{}", converted.body);
    let view = server
        .get(
            converted.header("location").ok_or("no location")?,
            Some(&cookie),
        )
        .await?;
    assert!(
        view.body
            .contains("Converted JD-2 to a multi-file document at revision 2."),
        "{}",
        view.body
    );
    assert!(latest_by_web(&server, &cookie, "/admin/documents/JD-2/history").await?);
    let twice = server
        .post_form("/admin/documents/JD-2/convert", &cookie, Some(&csrf), &[])
        .await?;
    assert_eq!(twice.status, 400);
    assert!(
        twice.body.contains("JD-2 is already a multi-file document"),
        "{}",
        twice.body
    );

    server.stop().await
}

/// [`structure_fixture`] with JD-2 converted, so files can move to it.
fn move_fixture(store: &mut Store) -> TestResult<()> {
    structure_fixture(store)?;
    store.convert(
        &joedocs::store::Actor::new("laptop-key", "claude-code"),
        "JD-2",
    )?;
    Ok(())
}

#[tokio::test]
async fn files_move_between_documents_with_the_messages_of_move_file() -> TestResult<()> {
    let server = start_full(|_| {}, move_fixture).await?;
    let cookie = server.login().await?;
    let csrf = server.csrf(&cookie).await?;
    let moved = server
        .post_form(
            "/admin/documents/JD-3/move-file",
            &cookie,
            Some(&csrf),
            &[("file", "items/first"), ("to", "JD-2"), ("as", "")],
        )
        .await?;
    assert_eq!(moved.status, 303, "{}", moved.body);
    let location = moved.header("location").ok_or("no location")?.to_owned();
    assert_eq!(
        location,
        "/admin/documents/JD-2?file=items%2Ffirst&done=moved&from=JD-3"
    );
    let target = server.get(&location, Some(&cookie)).await?;
    assert!(
        target
            .body
            .contains("Moved here from JD-3; JD-2 is now at revision 3."),
        "{}",
        target.body
    );
    assert!(latest_by_web(&server, &cookie, "/admin/documents/JD-2/history").await?);
    for (fields, message) in [
        (
            [
                ("file", "items/second"),
                ("to", "JD-2"),
                ("as", "items/first"),
            ],
            "file items/first in JD-2 already exists",
        ),
        (
            [("file", "items/second"), ("to", "JD-1"), ("as", "")],
            "JD-1 is a single-file document",
        ),
        (
            [("file", "items/second"), ("to", "nope"), ("as", "")],
            "\"nope\" is not a document ID like JD-42",
        ),
    ] {
        let refused = server
            .post_form(
                "/admin/documents/JD-3/move-file",
                &cookie,
                Some(&csrf),
                &fields,
            )
            .await?;
        assert!(refused.status >= 400 && refused.status < 500, "{fields:?}");
        assert!(
            refused.body.contains(&escaped(message)),
            "{fields:?}: {}",
            refused.body
        );
        assert!(
            refused.body.contains("href=\"/admin/documents/JD-3\""),
            "a way back: {}",
            refused.body
        );
    }
    let main = server
        .post_form(
            "/admin/documents/JD-3/move-file",
            &cookie,
            Some(&csrf),
            &[("file", "Backlog"), ("to", "JD-2"), ("as", "")],
        )
        .await?;
    assert!(
        main.status >= 400 && main.status < 500,
        "the main file cannot move: {}",
        main.body
    );
    server.stop().await
}

#[tokio::test]
async fn restore_file_brings_back_a_file_as_at_a_revision() -> TestResult<()> {
    let server = start_full(|_| {}, structure_fixture).await?;
    let cookie = server.login().await?;
    let csrf = server.csrf(&cookie).await?;
    let history = server
        .get(
            "/admin/documents/JD-3/history?file=items%2Ffirst",
            Some(&cookie),
        )
        .await?;
    assert!(
        history
            .body
            .contains("action=\"/admin/documents/JD-3/restore-file\""),
        "each row offers Restore: {}",
        history.body
    );
    let restored = server
        .post_form(
            "/admin/documents/JD-3/restore-file",
            &cookie,
            Some(&csrf),
            &[("file", "items/one"), ("version", "2")],
        )
        .await?;
    assert_eq!(restored.status, 303, "{}", restored.body);
    let location = restored.header("location").ok_or("no location")?.to_owned();
    assert_eq!(
        location,
        "/admin/documents/JD-3?file=items%2Fone&done=restored&from=2"
    );
    let view = server.get(&location, Some(&cookie)).await?;
    assert!(
        view.body
            .contains("Restored items/one as it was at revision 2; JD-3 is now at revision 6."),
        "{}",
        view.body
    );
    assert!(view.body.contains("Item one body."), "{}", view.body);
    assert!(latest_by_web(&server, &cookie, "/admin/documents/JD-3/history").await?);
    let again = server
        .post_form(
            "/admin/documents/JD-3/restore-file",
            &cookie,
            Some(&csrf),
            &[("file", "items/one"), ("version", "2")],
        )
        .await?;
    assert_eq!(again.status, 400, "{}", again.body);
    assert!(
        again
            .body
            .contains("items/one is already as it was at revision 2"),
        "{}",
        again.body
    );
    let bad = server
        .post_form(
            "/admin/documents/JD-3/restore-file",
            &cookie,
            Some(&csrf),
            &[("file", "items/one"), ("version", "two")],
        )
        .await?;
    assert_eq!(bad.status, 400);
    server.stop().await
}

// --- STEP-12: maintenance ----------------------------------------------------------

#[tokio::test]
async fn check_reports_problems_with_the_advice_joedocs_check_prints() -> TestResult<()> {
    let server = start_full(|_| {}, fixture).await?;
    let cookie = server.login().await?;
    let csrf = server.csrf(&cookie).await?;
    let page = server.get("/admin/maintenance", Some(&cookie)).await?;
    assert_eq!(page.status, 200, "{}", page.body);
    for expected in [
        "action=\"/admin/maintenance/check\"",
        "joedocs admin-token create",
        "joedocs backup --restore",
        "docs/deployment.md",
    ] {
        assert!(page.body.contains(expected), "{expected}: {}", page.body);
    }
    let clean = server
        .post_form("/admin/maintenance/check", &cookie, Some(&csrf), &[])
        .await?;
    assert_eq!(clean.status, 200, "{}", clean.body);
    assert!(clean.body.contains("No problems found."), "{}", clean.body);
    assert!(clean.body.contains("0 problems"), "{}", clean.body);

    let plan = server
        .dir
        .path()
        .join("vault/joedocs/plans/Rollout plan.md");
    let mut text = std::fs::read_to_string(&plan)?;
    text.push_str("\nEdited by hand.\n");
    std::fs::write(&plan, text)?;
    let drifted = server
        .post_form("/admin/maintenance/check", &cookie, Some(&csrf), &[])
        .await?;
    assert_eq!(drifted.status, 200, "{}", drifted.body);
    for expected in [
        "1 problem",
        "Vault files that are missing or differ from the history",
        "JD-1  joedocs/plans/Rollout plan.md",
        "restore --source=HEAD",
    ] {
        assert!(
            drifted.body.contains(&escaped(expected)),
            "{expected}: {}",
            drifted.body
        );
    }
    let tools: Vec<String> = server.audit()?.into_iter().map(|(t, _, _)| t).collect();
    assert!(tools.iter().any(|t| t == "admin:check"), "{tools:?}");
    server.stop().await
}

#[tokio::test]
async fn reindex_needs_its_confirmation_and_shows_its_report() -> TestResult<()> {
    let server = start_full(|_| {}, fixture).await?;
    let cookie = server.login().await?;
    let csrf = server.csrf(&cookie).await?;
    let page = server.get("/admin/maintenance", Some(&cookie)).await?;
    for expected in [
        "commandfor=\"reindex-dialog\" command=\"show-modal\"",
        "<dialog id=\"reindex-dialog\"",
        "action=\"/admin/maintenance/reindex\"",
        "data-busy=",
    ] {
        assert!(page.body.contains(expected), "{expected}: {}", page.body);
    }
    let refused = server
        .post_form("/admin/maintenance/reindex", &cookie, None, &[])
        .await?;
    assert_eq!(refused.status, 403);
    let rebuilt = server
        .post_form("/admin/maintenance/reindex", &cookie, Some(&csrf), &[])
        .await?;
    assert_eq!(rebuilt.status, 200, "{}", rebuilt.body);
    assert!(
        rebuilt
            .body
            .contains("Rebuilt the index from the history: 4 documents (1 archived)"),
        "{}",
        rebuilt.body
    );
    let tools: Vec<String> = server.audit()?.into_iter().map(|(t, _, _)| t).collect();
    assert!(tools.iter().any(|t| t == "admin:reindex"), "{tools:?}");
    server.stop().await
}

#[tokio::test]
async fn a_backup_downloads_with_a_readable_manifest() -> TestResult<()> {
    use std::io::Read as _;
    let server = start_full(|_| {}, fixture).await?;
    let cookie = server.login().await?;
    let csrf = server.csrf(&cookie).await?;
    let page = server.get("/admin/maintenance", Some(&cookie)).await?;
    assert!(page.body.contains("4 documents"), "{}", page.body);
    let refused = server
        .post_form("/admin/maintenance/backup", &cookie, None, &[])
        .await?;
    assert_eq!(refused.status, 403);
    let backup = server
        .post_form("/admin/maintenance/backup", &cookie, Some(&csrf), &[])
        .await?;
    assert_eq!(backup.status, 200, "{}", backup.body);
    assert_eq!(backup.header("content-type"), Some("application/gzip"));
    assert_eq!(
        backup.header("content-disposition"),
        Some("attachment; filename=\"joedocs-backup.tgz\"")
    );
    let documents = backup
        .header("joedocs-documents")
        .ok_or("no count")?
        .to_owned();
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(backup.bytes.as_slice()));
    let mut manifest = None;
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.path()?.to_string_lossy() == "manifest.json" {
            let mut text = String::new();
            entry.read_to_string(&mut text)?;
            manifest = Some(serde_json::from_str::<serde_json::Value>(&text)?);
        }
    }
    let manifest = manifest.ok_or("no manifest.json")?;
    assert_eq!(manifest["documents"].to_string(), documents);
    let rows = server.audit()?;
    let backups: Vec<_> = rows
        .iter()
        .filter(|(t, _, _)| t == "admin:backup")
        .collect();
    assert_eq!(backups.len(), 2, "{rows:?}");
    assert_eq!(backups[0].2, "forbidden");
    assert_eq!(backups[1].2, "ok");
    assert!(backups.iter().all(|(_, client, _)| client == "joedocs-web"));
    server.stop().await
}

// --- STEP-13: accessibility --------------------------------------------------------

/// One start tag: its name and attributes (names lower-cased).
struct Tag {
    name: String,
    attributes: Vec<(String, String)>,
}

impl Tag {
    fn get(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Every start tag of `html`, read with quoted attribute values.
fn tags(html: &str) -> Vec<Tag> {
    let bytes = html.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(offset) = html[i..].find('<') {
        i += offset + 1;
        if !bytes.get(i).is_some_and(u8::is_ascii_alphabetic) {
            continue;
        }
        let name_end = html[i..]
            .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
            .map_or(html.len(), |e| i + e);
        let name = html[i..name_end].to_ascii_lowercase();
        let mut attributes = Vec::new();
        let mut j = name_end;
        loop {
            while bytes
                .get(j)
                .is_some_and(|b| b.is_ascii_whitespace() || *b == b'/')
            {
                j += 1;
            }
            match bytes.get(j) {
                None => break,
                Some(b'>') => {
                    j += 1;
                    break;
                }
                Some(_) => {}
            }
            let start = j;
            while bytes
                .get(j)
                .is_some_and(|b| !b.is_ascii_whitespace() && !matches!(b, b'=' | b'>' | b'/'))
            {
                j += 1;
            }
            let attribute = html[start..j].to_ascii_lowercase();
            let mut value = String::new();
            if bytes.get(j) == Some(&b'=') {
                j += 1;
                match bytes.get(j) {
                    Some(&quote) if quote == b'"' || quote == b'\'' => {
                        let end = html[j + 1..]
                            .find(quote as char)
                            .map_or(html.len(), |e| j + 1 + e);
                        html[j + 1..end].clone_into(&mut value);
                        j = end + 1;
                    }
                    _ => {
                        let start = j;
                        while bytes
                            .get(j)
                            .is_some_and(|b| !b.is_ascii_whitespace() && *b != b'>')
                        {
                            j += 1;
                        }
                        html[start..j].clone_into(&mut value);
                    }
                }
            }
            if attribute.is_empty() {
                j += 1;
            } else {
                attributes.push((attribute, value));
            }
        }
        out.push(Tag { name, attributes });
        i = j.min(html.len());
    }
    out
}

/// What a page breaks of the structural checks of STEP-13.
fn html_problems(html: &str) -> Vec<String> {
    let tags = tags(html);
    let mut problems = Vec::new();
    let h1 = tags.iter().filter(|t| t.name == "h1").count();
    if h1 != 1 {
        problems.push(format!("{h1} <h1> elements"));
    }
    if !tags
        .iter()
        .any(|t| t.name == "html" && t.get("lang").is_some_and(|l| !l.is_empty()))
    {
        problems.push("no lang on <html>".to_owned());
    }
    for tag in &tags {
        for (name, _) in &tag.attributes {
            if name == "style" || name.starts_with("on") {
                problems.push(format!("<{} {name}=...>", tag.name));
            }
        }
        match tag.name.as_str() {
            "script" if tag.get("src").is_none() => problems.push("inline <script>".to_owned()),
            "img" if tag.get("alt").is_none() => {
                problems.push(format!("<img> without alt: {:?}", tag.get("src")));
            }
            "input" | "select" | "textarea" => {
                let kind = tag.get("type").unwrap_or("text");
                if matches!(kind, "hidden" | "submit" | "button" | "reset") {
                    continue;
                }
                let labelled = tag.get("aria-label").is_some()
                    || tag.get("aria-labelledby").is_some()
                    || tag
                        .get("id")
                        .is_some_and(|id| html.contains(&format!("for=\"{id}\"")));
                if !labelled {
                    problems.push(format!(
                        "<{} name={:?}> has no label",
                        tag.name,
                        tag.get("name")
                    ));
                }
            }
            "button"
                if tag.get("data-dialog-open").is_some()
                    || tag.get("data-dialog-close").is_some() =>
            {
                problems.push(format!(
                    "a dialog button needs JavaScript: {:?}",
                    tag.attributes
                ));
            }
            _ => {}
        }
    }
    for table in html.split("<table").skip(1) {
        let table = table.split("</table>").next().unwrap_or("");
        if !table.contains("<th") {
            problems.push("a table without headers".to_owned());
        }
    }
    problems
}

#[tokio::test]
async fn every_page_passes_the_structural_accessibility_checks() -> TestResult<()> {
    let server = start_full(|_| {}, structure_fixture).await?;
    let mut pages = vec![(
        "/admin/login".to_owned(),
        server.get("/admin/login", None).await?,
    )];
    let cookie = server.login().await?;
    let csrf = server.csrf(&cookie).await?;
    for path in [
        "/admin/",
        "/admin/documents",
        "/admin/documents/JD-1",
        "/admin/documents/JD-1?view=source",
        "/admin/documents/JD-2",
        "/admin/documents/JD-3",
        "/admin/documents/JD-3?file=items%2Ffirst",
        "/admin/documents/JD-3/history",
        "/admin/documents/JD-3/history?file=items%2Ffirst",
        "/admin/documents/JD-1/diff?from=1",
        "/admin/documents/JD-4",
        "/admin/search",
        "/admin/search?q=gap+OR+body",
        "/admin/search?q=-gap",
        "/admin/projects",
        "/admin/keys",
        "/admin/audit",
        "/admin/archive",
        "/admin/maintenance",
        "/admin/nowhere",
    ] {
        // A plain GET, without any htmx header, gives the whole page.
        pages.push((path.to_owned(), server.get(path, Some(&cookie)).await?));
    }
    for (path, fields) in [
        (
            "/admin/keys",
            vec![("name", "ci"), ("grant.joedocs", "read")],
        ),
        ("/admin/maintenance/check", vec![]),
        ("/admin/maintenance/reindex", vec![]),
        ("/admin/projects", vec![("name", "Bad Name"), ("key", "BN")]),
    ] {
        pages.push((
            format!("POST {path}"),
            server
                .post_form(path, &cookie, Some(&csrf), &fields)
                .await?,
        ));
    }
    let keys = server.get("/admin/keys", Some(&cookie)).await?;
    pages.push(("/admin/keys (with a key)".to_owned(), keys));
    let mut failures = Vec::new();
    for (path, reply) in &pages {
        assert!(reply.body.contains("<html"), "{path} is a whole page");
        let problems = html_problems(&reply.body);
        if !problems.is_empty() {
            failures.push(format!("{path}: {problems:?}"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
    server.stop().await
}

// --- JD-34 STEP-01: refusals are audited with their code ----------------------------

/// The one audit row written since `before` rows: `(tool, outcome)`.
fn only_row_since(server: &Server, before: usize) -> TestResult<(String, String)> {
    let rows = server.audit()?;
    let new = rows.get(before..).ok_or("rows went missing")?;
    assert_eq!(new.len(), 1, "one row per request: {new:?}");
    let (tool, client, outcome) = new[0].clone();
    assert_eq!(client, "joedocs-web");
    Ok((tool, outcome))
}

/// A refused request: path, form fields, status, message and audit code.
type Refusal<'a> = (&'a str, Vec<(&'a str, &'a str)>, u16, &'a str, &'a str);

/// Whether `/admin/audit?outcome_not=ok` lists each `tool - outcome` entry.
async fn refusals_list(server: &Server, cookie: &str, entries: &[&str]) -> TestResult<()> {
    let refusals = server
        .get(
            "/admin/audit?outcome_not=ok&key=laptop&limit=50",
            Some(cookie),
        )
        .await?;
    for entry in entries {
        assert!(
            refusals.body.contains(&format!("data-entry=\"{entry}\"")),
            "{entry}: {}",
            refusals.body
        );
    }
    Ok(())
}

#[tokio::test]
async fn refused_forms_are_audited_with_their_code_and_keep_their_page() -> TestResult<()> {
    let server = start_full(|_| {}, audit_fixture).await?;
    let cookie = server.login().await?;
    let csrf = server.csrf(&cookie).await?;
    let posts: [Refusal<'_>; 5] = [
        (
            "/admin/projects",
            vec![("name", "Bad Name"), ("key", "BN")],
            400,
            "must be lower-case letters",
            "invalid_argument",
        ),
        (
            "/admin/projects",
            vec![("name", "joedocs"), ("key", "ZZ")],
            409,
            "project joedocs already exists",
            "already_exists",
        ),
        (
            "/admin/keys",
            vec![("name", "Bad Name")],
            400,
            "must be lower-case letters",
            "invalid_argument",
        ),
        (
            "/admin/keys/ci/ungrant",
            vec![("project", "joedocs")],
            400,
            "API key ci has no grant on joedocs",
            "invalid_argument",
        ),
        (
            "/admin/keys/nope/rotate",
            vec![],
            404,
            "API key nope does not exist",
            "not_found",
        ),
    ];
    for (path, fields, status, message, code) in posts {
        let before = server.audit()?.len();
        let reply = server
            .post_form(path, &cookie, Some(&csrf), &fields)
            .await?;
        assert_eq!(reply.status, status, "{path} {fields:?}: {}", reply.body);
        assert!(reply.body.contains(message), "{path}: {}", reply.body);
        assert!(
            reply.body.contains("<form"),
            "the page stays usable: {path}"
        );
        assert_eq!(
            only_row_since(&server, before)?.1,
            code,
            "{path} {fields:?}"
        );
    }
    refusals_list(
        &server,
        &cookie,
        &[
            "admin:projects.add - already_exists",
            "admin:keys.create - invalid_argument",
            "admin:keys.ungrant - invalid_argument",
            "admin:keys.rotate - not_found",
        ],
    )
    .await?;
    // Successes and CSRF refusals keep their outcomes.
    let before = server.audit()?.len();
    let added = server
        .post_form(
            "/admin/projects",
            &cookie,
            Some(&csrf),
            &[("name", "alpha"), ("key", "AL")],
        )
        .await?;
    assert_eq!(added.status, 303);
    assert_eq!(only_row_since(&server, before)?.1, "ok");
    let before = server.audit()?.len();
    let forged = server
        .post_form(
            "/admin/projects",
            &cookie,
            None,
            &[("name", "beta"), ("key", "BE")],
        )
        .await?;
    assert_eq!(forged.status, 403);
    assert_eq!(only_row_since(&server, before)?.1, "forbidden");
    server.stop().await
}

#[tokio::test]
async fn refused_searches_and_audit_filters_are_audited_with_their_code() -> TestResult<()> {
    let server = start_full(|_| {}, audit_fixture).await?;
    let cookie = server.login().await?;
    let fragment = format!("Cookie: {cookie}\r\nHX-Request: true\r\nHX-Request-Type: partial\r\n");
    for (path, headers, status, message, code) in [
        (
            "/admin/search?q=-gap",
            None,
            400,
            "needs at least one word",
            "invalid_argument",
        ),
        (
            "/admin/search?q=-gap",
            Some(fragment.as_str()),
            400,
            "needs at least one word",
            "invalid_argument",
        ),
        (
            "/admin/search?project=nope",
            None,
            404,
            "project nope does not exist",
            "not_found",
        ),
        (
            "/admin/audit?since=yesterday",
            None,
            400,
            "is not a UTC date",
            "invalid_argument",
        ),
        (
            "/admin/audit?limit=0",
            None,
            400,
            "must be 1 to",
            "invalid_argument",
        ),
    ] {
        let before = server.audit()?.len();
        let reply = match headers {
            Some(headers) => server.send("GET", path, headers, "").await?,
            None => server.get(path, Some(&cookie)).await?,
        };
        assert_eq!(reply.status, status, "{path}: {}", reply.body);
        assert!(reply.body.contains(message), "{path}: {}", reply.body);
        assert_eq!(
            reply.body.contains("<html"),
            headers.is_none(),
            "{path}: page or fragment"
        );
        assert_eq!(only_row_since(&server, before)?.1, code, "{path}");
    }
    refusals_list(
        &server,
        &cookie,
        &[
            "admin:documents.search - invalid_argument",
            "admin:documents.search - not_found",
            "admin:audit.list - invalid_argument",
        ],
    )
    .await?;
    let before = server.audit()?.len();
    let found = server.get("/admin/search?q=gap", Some(&cookie)).await?;
    assert_eq!(found.status, 200);
    assert_eq!(only_row_since(&server, before)?.1, "ok");
    server.stop().await
}

// --- JD-34 STEP-02: an older revision shows its own facts ----------------------------

/// JD-1 created as a `draft` note "Draft title" by `laptop-key`, then
/// approved (r2), renamed "Final title" (r3) and moved to `plans` (r4) by
/// `ci-key` from `opencode`; JD-2 a multi-file note whose file `items/a`
/// came at r2, approved at r3.
fn facts_fixture(store: &mut Store) -> TestResult<()> {
    use joedocs::store::{Actor, NewDocument, NewFile};
    let first = Actor::new("laptop-key", "claude-code");
    let later = Actor::new("ci-key", "opencode");
    let approved = |store: &mut Store, id: &str| {
        let mut set = joedocs::document::front_matter::Fields::new();
        set.insert("status".into(), serde_json::json!("approved"));
        store.change_front_matter(&later, id, &set, &[], None)
    };
    store.add_project("joedocs", "JD", Some("JoeDocs"))?;
    store.create(
        &first,
        NewDocument {
            project: "joedocs".into(),
            doc_type: Some("notes".into()),
            title: "Draft title".into(),
            content: "The first text.\n".into(),
            ..NewDocument::default()
        },
    )?;
    approved(store, "JD-1")?;
    store.rename(&later, "JD-1", "Final title", None)?;
    store.move_document(&later, "JD-1", "plans", None)?;
    store.create(
        &first,
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
        &first,
        &NewFile {
            document_id: "JD-2".into(),
            file: "items/a".into(),
            content: "Item a.\n".into(),
            front_matter: joedocs::document::front_matter::Fields::new(),
            expected_revision: None,
            summary: None,
        },
    )?;
    approved(store, "JD-2")?;
    Ok(())
}

#[tokio::test]
async fn an_older_revision_shows_its_own_title_status_type_and_facts() -> TestResult<()> {
    let server = start_full(|_| {}, facts_fixture).await?;
    let cookie = server.login().await?;
    let source = server
        .get("/admin/documents/JD-1?version=1&view=source", Some(&cookie))
        .await?;
    for stored in [
        "title: Draft title",
        "type: notes",
        "status: draft",
        "updated_by: laptop-key",
        "client: claude-code",
    ] {
        assert!(
            source.body.contains(stored),
            "revision 1 stores {stored}: {}",
            source.body
        );
    }
    let old = server
        .get("/admin/documents/JD-1?version=1", Some(&cookie))
        .await?;
    assert_eq!(old.status, 200, "{}", old.body);
    for shown in [
        "<title>Draft title - JoeDocs</title>",
        "<h1>Draft title</h1>",
        "<span class=\"badge badge-status-draft\">draft</span>",
        "<dt>Type</dt><dd>notes</dd>",
        "by laptop-key (claude-code)",
    ] {
        assert!(
            old.body.contains(shown),
            "revision 1 shows {shown}: {}",
            old.body
        );
    }
    for absent in [
        "Final title",
        "badge-status-approved",
        "<dd>plans</dd>",
        "ci-key",
    ] {
        assert!(
            !old.body.contains(absent),
            "revision 1 does not show {absent}"
        );
    }
    let latest = server.get("/admin/documents/JD-1", Some(&cookie)).await?;
    for shown in [
        "<title>Final title - JoeDocs</title>",
        "<h1>Final title</h1>",
        "<span class=\"badge badge-status-approved\">approved</span>",
        "<dt>Type</dt><dd>plans</dd>",
        "by ci-key (opencode)",
    ] {
        assert!(
            latest.body.contains(shown),
            "the latest shows {shown}: {}",
            latest.body
        );
    }
    assert!(!latest.body.contains("data-facts=\"current\""));
    // A file at an older revision does not hold the document's title and
    // status, so those are labelled as current.
    let file = server
        .get(
            "/admin/documents/JD-2?file=items%2Fa&version=2",
            Some(&cookie),
        )
        .await?;
    assert_eq!(file.status, 200, "{}", file.body);
    assert!(
        file.body.contains("data-facts=\"current\""),
        "{}",
        file.body
    );
    let file_now = server
        .get("/admin/documents/JD-2?file=items%2Fa", Some(&cookie))
        .await?;
    assert!(!file_now.body.contains("data-facts=\"current\""));
    server.stop().await
}

// --- User sign-in and TOTP setup (0.5.1, JD-37 STEP-11) -------------------

const USER_PASSWORD: &str = "a long passphrase of my own";

/// Starts a server with `edit` applied and the user `alice` created;
/// returns her one-time password too.
async fn start_with_user(edit: impl FnOnce(&mut Config)) -> TestResult<(Server, String)> {
    let initial = Arc::new(std::sync::Mutex::new(String::new()));
    let keep = Arc::clone(&initial);
    let server = start_full(edit, move |store| {
        let password = joedocs::password::initial_password(&joedocs::auth::OsRandom)?;
        let hash = joedocs::password::hash(&password, &joedocs::auth::OsRandom)?;
        store.create_user("alice", &hash, &joedocs::auth::OsRandom)?;
        if let Ok(mut slot) = keep.lock() {
            password.expose_secret().clone_into(&mut slot);
        }
        Ok(())
    })
    .await?;
    let password = initial.lock().map(|p| p.clone()).unwrap_or_default();
    Ok((server, password))
}

impl Server {
    /// Posts `fields` from this server's own origin, with `cookie` if any.
    async fn post_public(
        &self,
        path: &str,
        cookie: Option<&str>,
        fields: &[(&str, &str)],
    ) -> TestResult<Reply> {
        let cookie = cookie.map_or_else(String::new, |c| format!("Cookie: {c}\r\n"));
        let headers = format!(
            "{cookie}Origin: {}\r\nSec-Fetch-Site: same-origin\r\n",
            self.origin()
        );
        self.send("POST", path, &headers, &form(fields)).await
    }
}

/// The `name=value` part of the first cookie `reply` sets named `name`.
fn set_cookie(reply: &Reply, name: &str) -> Option<String> {
    reply
        .cookies()
        .into_iter()
        .find(|c| c.starts_with(&format!("{name}=")) && !c.contains("Max-Age=0"))
        .and_then(|c| c.split(';').next())
        .map(str::to_owned)
}

/// The TOTP secret shown on a setup page.
fn shown_secret(body: &str) -> Option<Vec<u8>> {
    let start = body.find("id=\"totp-secret\">")? + "id=\"totp-secret\">".len();
    let end = body[start..].find('<')?;
    joedocs::totp::base32_decode(&body[start..start + end])
}

fn code_for(server: &Server, secret: &[u8]) -> String {
    joedocs::totp::code(
        secret,
        joedocs::totp::step_at(server.clock.now().as_second()),
    )
}

#[tokio::test]
async fn a_user_signs_in_on_the_web_after_setting_up_totp_with_a_qr_code() -> TestResult<()> {
    let (server, initial) = start_with_user(|_| {}).await?;
    let page = server.get("/admin/login", None).await?;
    assert!(
        page.body.contains("action=\"/admin/sign-in\""),
        "{}",
        page.body
    );
    assert!(
        page.body.contains("name=\"token\""),
        "the token form while token_login is on"
    );
    assert_eq!(html_problems(&page.body), Vec::<String>::new());
    let signed = server
        .post_public(
            "/admin/sign-in",
            None,
            &[("user", "alice"), ("password", &initial)],
        )
        .await?;
    assert_eq!(signed.status, 303, "{}", signed.body);
    assert_eq!(signed.header("location"), Some("/admin/setup"));
    let ticket = set_cookie(&signed, "joedocs_setup").ok_or("no setup cookie")?;
    assert!(ticket.contains("=jdt_"), "{ticket}");
    assert!(
        set_cookie(&signed, "joedocs_session").is_none(),
        "no session before setup"
    );
    let setup = server.get("/admin/setup", Some(&ticket)).await?;
    assert_eq!(setup.status, 200, "{}", setup.body);
    assert!(setup.body.contains("<svg"), "an inline QR code");
    assert!(
        setup.body.contains("otpauth://totp/JoeDocs:alice?"),
        "{}",
        setup.body
    );
    assert_eq!(setup.header("content-security-policy"), Some(CSP));
    assert_eq!(html_problems(&setup.body), Vec::<String>::new());
    let secret = shown_secret(&setup.body).ok_or("no secret on the page")?;
    let done = server
        .post_public(
            "/admin/setup",
            Some(&ticket),
            &[
                ("code", &code_for(&server, &secret)),
                ("password", USER_PASSWORD),
                ("password_again", USER_PASSWORD),
            ],
        )
        .await?;
    assert_eq!(done.status, 303, "{}", done.body);
    let cookie = set_cookie(&done, "joedocs_session").ok_or("no session cookie")?;
    assert!(cookie.contains("=jdv_"), "{cookie}");
    let home = server.get("/admin/", Some(&cookie)).await?;
    assert_eq!(home.status, 200, "{}", home.body);
    assert!(home.body.contains("alice"), "the chrome names the user");
    // Later sign-ins take the password and a code.
    server.clock.advance(30);
    let again = server
        .post_public(
            "/admin/sign-in",
            None,
            &[
                ("user", "alice"),
                ("password", USER_PASSWORD),
                ("code", &code_for(&server, &secret)),
            ],
        )
        .await?;
    assert_eq!(again.status, 303, "{}", again.body);
    assert!(set_cookie(&again, "joedocs_session").is_some());
    let audit = server.audit()?;
    for tool in [
        "admin:auth.login",
        "admin:web.setup_page",
        "admin:auth.setup",
    ] {
        assert!(
            audit
                .iter()
                .any(|(t, c, o)| t == tool && c == "joedocs-web" && o == "ok"),
            "{tool}: {audit:?}"
        );
    }
    server.stop().await
}

#[tokio::test]
async fn setup_re_shows_its_page_for_a_weak_or_mismatched_password_or_a_wrong_code()
-> TestResult<()> {
    let (server, initial) = start_with_user(|_| {}).await?;
    let signed = server
        .post_public(
            "/admin/sign-in",
            None,
            &[("user", "alice"), ("password", &initial)],
        )
        .await?;
    let ticket = set_cookie(&signed, "joedocs_setup").ok_or("no setup cookie")?;
    let setup = server.get("/admin/setup", Some(&ticket)).await?;
    let secret = shown_secret(&setup.body).ok_or("no secret")?;
    let code = code_for(&server, &secret);
    let mismatched = server
        .post_public(
            "/admin/setup",
            Some(&ticket),
            &[
                ("code", &code),
                ("password", USER_PASSWORD),
                ("password_again", "something else entirely"),
            ],
        )
        .await?;
    assert_eq!(mismatched.status, 400);
    assert!(mismatched.body.contains("differ"), "{}", mismatched.body);
    let weak = server
        .post_public(
            "/admin/setup",
            Some(&ticket),
            &[
                ("code", &code),
                ("password", "alice"),
                ("password_again", "alice"),
            ],
        )
        .await?;
    assert_eq!(weak.status, 400);
    assert!(weak.body.contains("too short"), "{}", weak.body);
    let wrong = server
        .post_public(
            "/admin/setup",
            Some(&ticket),
            &[
                ("code", "000000"),
                ("password", USER_PASSWORD),
                ("password_again", USER_PASSWORD),
            ],
        )
        .await?;
    assert_eq!(wrong.status, 401);
    assert!(wrong.body.contains("not accepted"), "{}", wrong.body);
    assert!(wrong.body.contains("<svg"), "the same QR code again");
    // Without the ticket, the sign-in form.
    let lapsed = server.get("/admin/setup", None).await?;
    assert_eq!(lapsed.status, 401);
    assert!(lapsed.body.contains("action=\"/admin/sign-in\""));
    server.stop().await
}

#[tokio::test]
async fn sign_in_refusals_are_the_same_whatever_was_wrong_and_need_the_same_origin()
-> TestResult<()> {
    let (server, _initial) = start_with_user(|_| {}).await?;
    let mut bodies = Vec::new();
    for (user, password) in [
        ("alice", "not the password"),
        ("nobody", "not the password"),
    ] {
        let reply = server
            .post_public(
                "/admin/sign-in",
                None,
                &[("user", user), ("password", password), ("code", "123456")],
            )
            .await?;
        assert_eq!(reply.status, 401);
        bodies.push(reply.body);
    }
    assert_eq!(bodies[0], bodies[1]);
    let foreign = server
        .send(
            "POST",
            "/admin/sign-in",
            "Origin: https://evil.example\r\nSec-Fetch-Site: cross-site\r\n",
            &form(&[("user", "alice"), ("password", "x")]),
        )
        .await?;
    assert_eq!(foreign.status, 403);
    let audit = server.audit()?;
    let refused: Vec<_> = audit
        .iter()
        .filter(|(t, _, _)| t == "admin:auth.login")
        .collect();
    assert_eq!(refused.len(), 3, "{audit:?}");
    server.stop().await
}

#[tokio::test]
async fn switching_token_login_off_hides_and_refuses_the_token_form_and_its_sessions()
-> TestResult<()> {
    // A token web session opened in the store before the server starts.
    let opened = Arc::new(std::sync::Mutex::new(String::new()));
    let keep = Arc::clone(&opened);
    let server = start_full(
        |c| c.admin.token_login = false,
        move |store| {
            let token = store.create_admin_token("old", &joedocs::auth::OsRandom)?;
            let secret = joedocs::totp::base32_decode(token.totp_secret.expose_secret())
                .ok_or("bad base32")?;
            let code = joedocs::totp::code(&secret, joedocs::totp::step_at(1_793_000_000));
            let session = store.web_login(&token.token, &code, 8, &joedocs::auth::OsRandom)?;
            if let Ok(mut slot) = keep.lock() {
                session.token.expose_secret().clone_into(&mut slot);
            }
            Ok(())
        },
    )
    .await?;
    let session = opened.lock().map(|s| s.clone()).unwrap_or_default();
    assert!(session.starts_with("jdw_"));
    let page = server.get("/admin/login", None).await?;
    assert!(!page.body.contains("name=\"token\""), "{}", page.body);
    let posted = server.post_login(&server.code(), "/admin/").await?;
    assert_eq!(posted.status, 401);
    assert!(posted.body.contains("admin.token_login"), "{}", posted.body);
    let refused = server
        .get("/admin/", Some(&format!("joedocs_session={session}")))
        .await?;
    assert_eq!(refused.status, 303, "a token session no longer opens pages");
    assert!(
        refused
            .header("location")
            .unwrap_or_default()
            .starts_with("/admin/login")
    );
    server.stop().await
}

// --- Account and Users pages (0.5.1, JD-37 STEP-12) ------------------------

/// Takes `alice` through her first sign-in on the web; returns her session
/// cookie, the CSRF token of her pages, and her TOTP secret.
async fn signed_in_user(server: &Server, initial: &str) -> TestResult<(String, String, Vec<u8>)> {
    let signed = server
        .post_public(
            "/admin/sign-in",
            None,
            &[("user", "alice"), ("password", initial)],
        )
        .await?;
    let ticket = set_cookie(&signed, "joedocs_setup").ok_or("no setup cookie")?;
    let setup = server.get("/admin/setup", Some(&ticket)).await?;
    let secret = shown_secret(&setup.body).ok_or("no secret")?;
    let done = server
        .post_public(
            "/admin/setup",
            Some(&ticket),
            &[
                ("code", &code_for(server, &secret)),
                ("password", USER_PASSWORD),
                ("password_again", USER_PASSWORD),
            ],
        )
        .await?;
    let cookie = set_cookie(&done, "joedocs_session").ok_or("no session cookie")?;
    server.clock.advance(30);
    let csrf = server.csrf(&cookie).await?;
    Ok((cookie, csrf, secret))
}

#[tokio::test]
async fn the_account_page_changes_the_password_and_revokes_devices() -> TestResult<()> {
    let (server, initial) = start_with_user(|_| {}).await?;
    let (cookie, csrf, secret) = signed_in_user(&server, &initial).await?;
    // A device signed in from the command line, through the admin API.
    let body = serde_json::json!({
        "user": "alice", "password": USER_PASSWORD, "code": code_for(&server, &secret), "device": "laptop"
    })
    .to_string();
    let api = server
        .send(
            "POST",
            "/admin/v1/auth/login",
            "content-type: application/json\r\n",
            &body,
        )
        .await?;
    assert_eq!(api.status, 200, "{}", api.body);
    server.clock.advance(30);
    let page = server.get("/admin/account", Some(&cookie)).await?;
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(page.body.contains("laptop"), "{}", page.body);
    assert_eq!(html_problems(&page.body), Vec::<String>::new());
    let mismatched = server
        .post_form(
            "/admin/account/password",
            &cookie,
            Some(&csrf),
            &[
                ("password", USER_PASSWORD),
                ("code", &code_for(&server, &secret)),
                ("new_password", "a fresh passphrase here"),
                ("new_password_again", "another one entirely"),
            ],
        )
        .await?;
    assert_eq!(mismatched.status, 400);
    assert!(mismatched.body.contains("differ"), "{}", mismatched.body);
    let wrong = server
        .post_form(
            "/admin/account/password",
            &cookie,
            Some(&csrf),
            &[
                ("password", "not my password at all"),
                ("code", "000000"),
                ("new_password", "a fresh passphrase here"),
                ("new_password_again", "a fresh passphrase here"),
            ],
        )
        .await?;
    assert_eq!(wrong.status, 401);
    assert!(wrong.body.contains("not right"), "{}", wrong.body);
    let changed = server
        .post_form(
            "/admin/account/password",
            &cookie,
            Some(&csrf),
            &[
                ("password", USER_PASSWORD),
                ("code", &code_for(&server, &secret)),
                ("new_password", "a fresh passphrase here"),
                ("new_password_again", "a fresh passphrase here"),
            ],
        )
        .await?;
    assert_eq!(changed.status, 303, "{}", changed.body);
    let after = server
        .get("/admin/account?changed=1", Some(&cookie))
        .await?;
    assert_eq!(after.status, 200, "this session stays signed in");
    assert!(
        !after.body.contains(">laptop<"),
        "the other device ended: {}",
        after.body
    );
    let audit = server.audit()?;
    assert!(
        audit
            .iter()
            .any(|(t, _, o)| t == "admin:me.password" && o == "ok"),
        "{audit:?}"
    );
    assert!(
        audit
            .iter()
            .any(|(t, _, o)| t == "admin:me.password" && o == "unauthorized")
    );
    server.stop().await
}

#[tokio::test]
async fn the_users_page_creates_disables_and_resets_users_but_not_the_last_one() -> TestResult<()> {
    let (server, initial) = start_with_user(|_| {}).await?;
    let (cookie, csrf, _) = signed_in_user(&server, &initial).await?;
    let page = server.get("/admin/users", Some(&cookie)).await?;
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(page.body.contains("alice"));
    assert_eq!(html_problems(&page.body), Vec::<String>::new());
    let created = server
        .post_form("/admin/users", &cookie, Some(&csrf), &[("name", "bob")])
        .await?;
    assert_eq!(created.status, 200, "{}", created.body);
    assert!(created.body.contains("Created user bob."));
    let start = created
        .body
        .find("id=\"one-time-password\">")
        .ok_or("no password")?
        + 23;
    let shown = &created.body[start..start + 23];
    assert_eq!(shown.matches('-').count(), 3, "{shown}");
    assert_eq!(html_problems(&created.body), Vec::<String>::new());
    let taken = server
        .post_form("/admin/users", &cookie, Some(&csrf), &[("name", "bob")])
        .await?;
    assert_eq!(taken.status, 409);
    assert!(taken.body.contains("already exists"), "{}", taken.body);
    let disabled = server
        .post_form("/admin/users/bob/disable", &cookie, Some(&csrf), &[])
        .await?;
    assert_eq!(disabled.status, 303, "{}", disabled.body);
    let listed = server.get("/admin/users", Some(&cookie)).await?;
    assert!(listed.body.contains("disabled"), "{}", listed.body);
    let reset = server
        .post_form("/admin/users/bob/reset-totp", &cookie, Some(&csrf), &[])
        .await?;
    assert_eq!(reset.status, 200, "{}", reset.body);
    assert!(reset.body.contains("Cleared the authenticator of bob"));
    // Alice is the only one who can sign in.
    for action in ["disable", "reset-password", "reset-totp"] {
        let refused = server
            .post_form(
                &format!("/admin/users/alice/{action}"),
                &cookie,
                Some(&csrf),
                &[],
            )
            .await?;
        assert_eq!(refused.status, 400, "{action}");
        assert!(
            refused.body.contains("last user who can sign in"),
            "{action}"
        );
    }
    let audit = server.audit()?;
    assert!(
        audit
            .iter()
            .any(|(t, _, o)| t == "admin:users.create" && o == "ok")
    );
    assert!(
        audit
            .iter()
            .any(|(t, _, o)| t == "admin:users.reset_totp" && o == "ok")
    );
    server.stop().await
}

#[tokio::test]
async fn admin_token_sessions_cannot_use_the_account_or_users_pages() -> TestResult<()> {
    let server = start().await?;
    let cookie = server.login().await?;
    let csrf = server.csrf(&cookie).await?;
    for path in ["/admin/account", "/admin/users"] {
        let page = server.get(path, Some(&cookie)).await?;
        assert_eq!(page.status, 403, "{path}");
        assert!(page.body.contains("signed-in user"), "{path}");
    }
    let created = server
        .post_form("/admin/users", &cookie, Some(&csrf), &[("name", "eve")])
        .await?;
    assert_eq!(created.status, 403);
    let outcomes: Vec<_> = server
        .audit()?
        .into_iter()
        .filter(|(t, _, _)| t.starts_with("admin:users") || t.starts_with("admin:me"))
        .map(|(_, _, o)| o)
        .collect();
    assert_eq!(
        outcomes,
        ["user_required", "user_required", "user_required"]
    );
    server.stop().await
}

// --- Review fixes (0.5.1, JD-39) --------------------------------------------

/// Signs `alice` in with the one-time password `initial` after a password
/// reset (her authenticator is kept), chooses a new password, and returns
/// the new session cookie.
async fn set_password_again(server: &Server, initial: &str, secret: &[u8]) -> TestResult<String> {
    server.clock.advance(30);
    let signed = server
        .post_public(
            "/admin/sign-in",
            None,
            &[
                ("user", "alice"),
                ("password", initial),
                ("code", &code_for(server, secret)),
            ],
        )
        .await?;
    let ticket = set_cookie(&signed, "joedocs_setup").ok_or("no setup cookie")?;
    let done = server
        .post_public(
            "/admin/setup",
            Some(&ticket),
            &[
                ("password", USER_PASSWORD),
                ("password_again", USER_PASSWORD),
            ],
        )
        .await?;
    let cookie = set_cookie(&done, "joedocs_session").ok_or("no session cookie")?;
    server.clock.advance(30);
    Ok(cookie)
}

/// Whether the page `/admin/` opens with `cookie`.
async fn opens(server: &Server, cookie: &str) -> TestResult<bool> {
    Ok(server.get("/admin/", Some(cookie)).await?.status == 200)
}

#[tokio::test]
async fn disabling_or_resetting_a_user_ends_their_browser_sessions_for_good() -> TestResult<()> {
    let (server, initial) = start_with_user(|_| {}).await?;
    let dir = server.dir.path().to_owned();
    let (first, _, secret) = signed_in_user(&server, &initial).await?;
    assert!(opens(&server, &first).await?);
    joedocs_cli(&dir, &["user", "disable", "alice"])?;
    joedocs_cli(&dir, &["user", "enable", "alice"])?;
    assert!(
        !opens(&server, &first).await?,
        "a cookie from before disable"
    );

    // Reset the password, then choose a new one: the old cookie stays ended.
    let reset = joedocs_cli(&dir, &["user", "reset-password", "alice"])?;
    let again = reset.lines().last().ok_or("no one-time password")?;
    let second = set_password_again(&server, again, &secret).await?;
    assert!(opens(&server, &second).await?);
    assert!(!opens(&server, &first).await?);

    // Reset the authenticator, then set it up again.
    let reset = joedocs_cli(&dir, &["user", "reset-totp", "alice"])?;
    let again = reset.lines().last().ok_or("no one-time password")?;
    let (third, _, _) = signed_in_user(&server, again).await?;
    assert!(opens(&server, &third).await?);
    assert!(
        !opens(&server, &second).await?,
        "a cookie from before reset-totp"
    );
    server.stop().await
}

// --- JD-41: 0.5.2 user experience --------------------------------------------------

/// Headers of an htmx request for part of a page.
fn htmx(cookie: &str) -> String {
    format!("Cookie: {cookie}\r\nHX-Request: true\r\nHX-Request-Type: partial\r\n")
}

#[tokio::test]
async fn filters_apply_on_change_and_htmx_gets_only_the_results() -> TestResult<()> {
    let server = start_full(|_| {}, audit_fixture).await?;
    let cookie = server.login().await?;
    // `(path, the form's htmx trigger, a filtered query, a row it lists,
    // a row it leaves out)`.
    let pages = [
        (
            "/admin/documents",
            "change, submit",
            "/admin/documents?project=joedocs&type=plans",
            "Rollout plan",
            "Second doc",
        ),
        (
            "/admin/archive",
            "change, submit",
            "/admin/archive?type=notes",
            "data-archived=\"JD-4\"",
            "No archived documents match",
        ),
        (
            "/admin/audit",
            "input changed delay:300ms, submit",
            "/admin/audit?key=ci-bot&tool=archive_document",
            "archive_document JD-4 ok",
            "new_document JD-2 ok",
        ),
    ];
    for (path, trigger, query, listed, absent) in pages {
        let page = server.get(path, Some(&cookie)).await?;
        assert_eq!(page.status, 200, "{path}: {}", page.body);
        for needle in [
            format!("hx-get=\"{path}\""),
            format!("hx-trigger=\"{trigger}\""),
            "hx-target=\"#results\"".to_owned(),
            "hx-push-url=\"true\"".to_owned(),
            "id=\"results\"".to_owned(),
            "data-focus=\"keep\"".to_owned(),
            // The button stays for Enter and for browsers without scripts.
            "class=\"filters-apply\"".to_owned(),
        ] {
            assert!(
                page.body.contains(&needle),
                "{path} {needle}: {}",
                page.body
            );
        }
        let fragment = server.send("GET", query, &htmx(&cookie), "").await?;
        assert_eq!(fragment.status, 200, "{query}: {}", fragment.body);
        assert!(
            !fragment.body.contains("<html"),
            "{query}: {}",
            fragment.body
        );
        assert!(
            !fragment.body.contains("class=\"filters"),
            "only the results, not the filters: {query}"
        );
        assert!(fragment.body.contains(listed), "{query}: {}", fragment.body);
        assert!(
            !fragment.body.contains(absent),
            "{query}: {}",
            fragment.body
        );
        let whole = server.get(query, Some(&cookie)).await?;
        assert!(whole.body.contains("<html"), "a plain request: {query}");
        assert!(whole.body.contains(listed), "{query}");
    }
    // The project line follows the project picked.
    let notes = server
        .send("GET", "/admin/documents?project=notes", &htmx(&cookie), "")
        .await?;
    assert!(notes.body.contains("<code>NT</code>"), "{}", notes.body);
    // A refused audit filter answers with its message as a fragment.
    let refused = server
        .send("GET", "/admin/audit?since=yesterday", &htmx(&cookie), "")
        .await?;
    assert_eq!(refused.status, 400, "{}", refused.body);
    assert!(!refused.body.contains("<html"), "{}", refused.body);
    assert!(refused.body.contains("notice error"), "{}", refused.body);
    server.stop().await
}

/// The titles of the Documents rows, in order.
fn titles(body: &str) -> Vec<String> {
    body.split("<td><a href=\"/admin/documents/")
        .skip(1)
        .filter_map(|part| {
            let start = part.find("\">")? + 2;
            let end = part[start..].find("</a>")?;
            Some(part[start..start + end].to_owned())
        })
        .collect()
}

#[tokio::test]
async fn documents_sort_by_their_column_headers_across_pages() -> TestResult<()> {
    let server = start_full(|_| {}, fixture).await?;
    let cookie = server.login().await?;
    let get = |query: &str| {
        let path = format!("/admin/documents?project=joedocs{query}");
        let cookie = cookie.clone();
        let server = &server;
        async move { server.get(&path, Some(&cookie)).await }
    };
    // Unsorted: every sortable header links to its ascending order; Files
    // is not sortable, and the Order select is gone.
    let plain = get("").await?;
    assert_eq!(plain.status, 200, "{}", plain.body);
    for column in ["id", "title", "type", "status", "revision", "updated"] {
        let link = format!("href=\"/admin/documents?project=joedocs&#38;sort={column}\"");
        assert!(plain.body.contains(&link), "{link}: {}", plain.body);
    }
    assert!(!plain.body.contains("sort=files"), "{}", plain.body);
    assert!(!plain.body.contains("id=\"f-sort\""), "{}", plain.body);
    assert!(!plain.body.contains("aria-sort"), "{}", plain.body);

    // Ascending, then descending, then back to the default order.
    let ascending = get("&sort=title").await?;
    assert_eq!(
        titles(&ascending.body),
        ["Backlog", "Rollout plan", "Second doc"]
    );
    assert!(
        ascending.body.contains("aria-sort=\"ascending\""),
        "{}",
        ascending.body
    );
    assert!(
        ascending
            .body
            .contains("href=\"/admin/documents?project=joedocs&#38;sort=title&#38;dir=desc\""),
        "{}",
        ascending.body
    );
    // The filter form keeps the order through a hidden field it owns.
    assert!(
        ascending
            .body
            .contains("name=\"sort\" value=\"title\" form=\"documents-filters\""),
        "{}",
        ascending.body
    );
    let descending = get("&sort=title&dir=desc").await?;
    assert_eq!(
        titles(&descending.body),
        ["Second doc", "Rollout plan", "Backlog"]
    );
    assert!(descending.body.contains("aria-sort=\"descending\""));
    let title_header = descending
        .body
        .split("<th")
        .find(|th| th.contains(">Title<"))
        .ok_or("no Title header")?;
    assert!(
        title_header.contains("href=\"/admin/documents?project=joedocs\""),
        "the third click goes back to the default order: {title_header}"
    );
    server.stop().await
}

#[tokio::test]
async fn each_column_order_pages_and_the_0_5_1_orders_still_work() -> TestResult<()> {
    let server = start_full(|_| {}, fixture).await?;
    let cookie = server.login().await?;
    let get = |query: &str| {
        let path = format!("/admin/documents?project=joedocs{query}");
        let cookie = cookie.clone();
        let server = &server;
        async move { server.get(&path, Some(&cookie)).await }
    };
    for (query, expected) in [
        ("&sort=id", ["Rollout plan", "Second doc", "Backlog"]),
        (
            "&sort=id&dir=desc",
            ["Backlog", "Second doc", "Rollout plan"],
        ),
        // Ties go by ID, in the same direction.
        (
            "&sort=revision&dir=desc",
            ["Backlog", "Rollout plan", "Second doc"],
        ),
        ("&sort=type", ["Second doc", "Backlog", "Rollout plan"]),
        ("&sort=status", ["Second doc", "Rollout plan", "Backlog"]),
        ("&sort=updated", ["Rollout plan", "Second doc", "Backlog"]),
    ] {
        let page = get(query).await?;
        assert_eq!(page.status, 200, "{query}: {}", page.body);
        assert_eq!(titles(&page.body), expected, "{query}");
    }

    // Header links keep the filters and the page size, and start at the
    // first page; the pager keeps the order.
    let paged = get("&type=notes&sort=title&dir=desc&start=2&limit=1").await?;
    assert_eq!(titles(&paged.body), ["Backlog"]);
    assert!(
        paged.body.contains(
            "href=\"/admin/documents?project=joedocs&#38;type=notes&#38;sort=id&#38;limit=1\""
        ),
        "{}",
        paged.body
    );
    assert!(
        paged.body.contains(
            "href=\"/admin/documents?project=joedocs&#38;type=notes&#38;sort=title&#38;dir=desc&#38;limit=1\""
        ),
        "the previous page keeps the order: {}",
        paged.body
    );

    // The 0.5.1 orders still work.
    for legacy in [
        "updated_desc",
        "updated_asc",
        "created_desc",
        "created_asc",
        "title",
    ] {
        let page = get(&format!("&sort={legacy}")).await?;
        assert_eq!(page.status, 200, "{legacy}: {}", page.body);
    }
    assert_eq!(
        titles(&get("&sort=created_desc").await?.body),
        ["Backlog", "Second doc", "Rollout plan"]
    );
    for bad in ["&sort=nope", "&sort=files", "&sort=title&dir=sideways"] {
        let page = get(bad).await?;
        assert_eq!(page.status, 400, "{bad}: {}", page.body);
    }
    server.stop().await
}

/// The first table in `body` after `from`, up to its end.
fn table_after<'a>(body: &'a str, from: &str) -> TestResult<&'a str> {
    let start = body.find(from).ok_or("no such place")?;
    let table = body[start..].find("<table").ok_or("no table")? + start;
    let end = body[table..].find("</table>").ok_or("no table end")? + table;
    Ok(&body[table..end])
}

/// Checks a table the browser sorts: marked, with `data-sort` on every
/// header but `unsorted`, and a sort value on every time cell.
fn assert_sortable(table: &str, unsorted: &[&str]) {
    assert!(table.starts_with("<table data-sortable"), "{table}");
    for th in table.split("<th ").skip(1) {
        let label = th
            .split_once('>')
            .map_or("", |(_, rest)| rest.split('<').next().unwrap_or(""));
        let marked = th.starts_with("scope=\"col\" data-sort");
        assert_eq!(marked, !unsorted.contains(&label), "{label}: {th}");
    }
    for cell in table.split("<td class=\"nowrap\"").skip(1) {
        assert!(
            cell.starts_with(" data-sort-value=\""),
            "a time cell: {cell}"
        );
    }
}

#[tokio::test]
async fn tables_that_show_every_row_sort_in_the_browser_and_paged_ones_do_not() -> TestResult<()> {
    let initial = Arc::new(std::sync::Mutex::new(String::new()));
    let keep = Arc::clone(&initial);
    let server = start_full(
        |_| {},
        move |store| {
            audit_fixture(store)?;
            let password = joedocs::password::initial_password(&joedocs::auth::OsRandom)?;
            let hash = joedocs::password::hash(&password, &joedocs::auth::OsRandom)?;
            store.create_user("alice", &hash, &joedocs::auth::OsRandom)?;
            if let Ok(mut slot) = keep.lock() {
                password.expose_secret().clone_into(&mut slot);
            }
            Ok(())
        },
    )
    .await?;
    let password = initial.lock().map(|p| p.clone()).unwrap_or_default();
    let (cookie, _, secret) = signed_in_user(&server, &password).await?;
    let body = serde_json::json!({
        "user": "alice", "password": USER_PASSWORD, "code": code_for(&server, &secret), "device": "laptop"
    })
    .to_string();
    let api = server
        .send(
            "POST",
            "/admin/v1/auth/login",
            "content-type: application/json\r\n",
            &body,
        )
        .await?;
    assert_eq!(api.status, 200, "{}", api.body);
    server.clock.advance(30);

    let page = |path: &'static str| {
        let cookie = cookie.clone();
        let server = &server;
        async move {
            let page = server.get(path, Some(&cookie)).await?;
            assert_eq!(page.status, 200, "{path}: {}", page.body);
            TestResult::Ok(page.body)
        }
    };
    let projects = page("/admin/projects").await?;
    assert_sortable(table_after(&projects, "<main")?, &[]);
    let keys = page("/admin/keys").await?;
    assert_sortable(table_after(&keys, "<main")?, &["Grants", "Actions"]);
    let users = page("/admin/users").await?;
    assert_sortable(table_after(&users, "<main")?, &["Actions"]);
    let account = page("/admin/account").await?;
    assert!(account.contains("laptop"), "{account}");
    assert_sortable(table_after(&account, "<main")?, &["Actions"]);
    let archive = page("/admin/archive").await?;
    assert_sortable(table_after(&archive, "id=\"results\"")?, &["Action"]);
    let overview = page("/admin/").await?;
    assert_sortable(table_after(&overview, "<main")?, &[]);
    // The Overview's recent activity stays newest first.
    let recent = overview.rfind("<table").ok_or("no second table")?;
    assert!(!overview[recent..].starts_with("<table data-sortable"));
    for path in [
        "/admin/audit",
        "/admin/search",
        "/admin/documents/JD-1/history",
    ] {
        let body = page(path).await?;
        assert!(!body.contains("data-sortable"), "{path}");
    }
    server.stop().await
}

#[tokio::test]
async fn a_document_id_in_the_search_opens_that_document() -> TestResult<()> {
    let server = start_full(|_| {}, fixture).await?;
    let cookie = server.login().await?;
    let before = server.audit()?.len();
    // The top bar sends a plain request: an exact ID of a live document,
    // in any case and with spaces around it, opens it.
    for query in ["JD-2", "jd-2", "%20jD-2%20"] {
        let reply = server
            .get(&format!("/admin/search?q={query}"), Some(&cookie))
            .await?;
        assert_eq!(reply.status, 303, "{query}: {}", reply.body);
        assert_eq!(
            reply.header("location"),
            Some("/admin/documents/JD-2"),
            "{query}"
        );
    }
    let rows = server.audit()?;
    assert!(
        rows[before..]
            .iter()
            .all(|(tool, _, outcome)| tool == "admin:documents.search" && outcome == "ok"),
        "{:?}",
        &rows[before..]
    );
    // Typing it into the live search lists it first and stays put.
    let live = server
        .send("GET", "/admin/search?q=jd-2", &htmx(&cookie), "")
        .await?;
    assert_eq!(live.status, 200, "{}", live.body);
    assert!(live.body.contains("class=\"id-match\""), "{}", live.body);
    let first = live
        .body
        .find("href=\"/admin/documents/")
        .ok_or("no link")?;
    assert!(
        live.body[first..].starts_with("href=\"/admin/documents/JD-2\""),
        "{}",
        live.body
    );
    assert!(live.body.contains("Second doc"), "{}", live.body);
    // An archived document's ID says where it went.
    for headers in [format!("Cookie: {cookie}\r\n"), htmx(&cookie)] {
        let archived = server
            .send("GET", "/admin/search?q=JD-4", &headers, "")
            .await?;
        assert_eq!(archived.status, 200, "{}", archived.body);
        assert!(
            archived.body.contains("JD-4 is archived"),
            "{}",
            archived.body
        );
        assert!(
            archived.body.contains("href=\"/admin/archive\""),
            "{}",
            archived.body
        );
    }
    // Anything else searches as before.
    for query in ["JD-99999", "JD-2%20plan", "JD-02", "rollout"] {
        let page = server
            .get(&format!("/admin/search?q={query}"), Some(&cookie))
            .await?;
        assert_ne!(page.status, 303, "{query}");
        assert!(!page.body.contains("class=\"id-match\""), "{query}");
    }
    server.stop().await
}

#[tokio::test]
async fn documents_are_archived_from_their_page_with_a_reason() -> TestResult<()> {
    let server = start_full(|_| {}, fixture).await?;
    let cookie = server.login().await?;
    let csrf = server.csrf(&cookie).await?;
    // The latest revision of a document, single-file or multi-file, offers
    // it; an older revision and a file page do not.
    for (path, offered) in [
        ("/admin/documents/JD-2", true),
        ("/admin/documents/JD-3", true),
        ("/admin/documents/JD-1?version=1", false),
        ("/admin/documents/JD-3?file=items%2Fone", false),
    ] {
        let page = server.get(path, Some(&cookie)).await?;
        assert_eq!(page.status, 200, "{path}: {}", page.body);
        assert_eq!(
            page.body.contains("commandfor=\"archive-dialog\""),
            offered,
            "{path}"
        );
        if offered {
            for needle in [
                "action=\"/admin/documents/",
                "/archive\"",
                "<label for=\"archive-reason\">",
                "id=\"archive-reason\" name=\"reason\" required",
            ] {
                assert!(page.body.contains(needle), "{path} {needle}: {}", page.body);
            }
        }
    }
    // Refused: no reason, a blank one, no CSRF token, an unknown document.
    for (path, fields, token) in [
        ("/admin/documents/JD-2/archive", vec![], Some(csrf.as_str())),
        (
            "/admin/documents/JD-2/archive",
            vec![("reason", "   ")],
            Some(csrf.as_str()),
        ),
        (
            "/admin/documents/JD-2/archive",
            vec![("reason", "done")],
            None,
        ),
        (
            "/admin/documents/JD-99/archive",
            vec![("reason", "done")],
            Some(csrf.as_str()),
        ),
    ] {
        let reply = server.post_form(path, &cookie, token, &fields).await?;
        assert!(
            (400..500).contains(&reply.status),
            "{path} {fields:?}: {} {}",
            reply.status,
            reply.body
        );
    }
    let still = server
        .get("/admin/documents?project=joedocs", Some(&cookie))
        .await?;
    assert!(still.body.contains("Second doc"), "nothing was archived");
    server.stop().await
}

#[tokio::test]
async fn an_archived_document_lands_on_the_archive_page_with_a_notice() -> TestResult<()> {
    let server = start_full(|_| {}, fixture).await?;
    let cookie = server.login().await?;
    let csrf = server.csrf(&cookie).await?;
    let before = server.audit()?.len();

    let archived = server
        .post_form(
            "/admin/documents/JD-2/archive",
            &cookie,
            Some(&csrf),
            &[("reason", "replaced by the backlog")],
        )
        .await?;
    assert_eq!(archived.status, 303, "{}", archived.body);
    let location = archived.header("location").ok_or("no location")?.to_owned();
    assert_eq!(location, "/admin/archive?done=archived&id=JD-2");
    let archive = server.get(&location, Some(&cookie)).await?;
    assert!(
        archive.body.contains("Archived JD-2."),
        "the notice names it: {}",
        archive.body
    );
    let row = row_of(&archive.body, "archived", "JD-2");
    assert!(row.contains("replaced by the backlog"), "{row}");
    let library = server
        .get("/admin/documents?project=joedocs", Some(&cookie))
        .await?;
    assert!(!library.body.contains("Second doc"), "{}", library.body);
    let rows = server.audit()?;
    let tools: Vec<_> = rows[before..]
        .iter()
        .filter(|(tool, _, _)| tool == "admin:document.archive")
        .map(|(_, client, outcome)| (client.as_str(), outcome.as_str()))
        .collect();
    assert_eq!(tools, [("joedocs-web", "ok")]);
    // Archiving it again is refused, and the page says why.
    let again = server
        .post_form(
            "/admin/documents/JD-2/archive",
            &cookie,
            Some(&csrf),
            &[("reason", "twice")],
        )
        .await?;
    assert!((400..500).contains(&again.status), "{}", again.body);
    assert!(again.body.contains("JD-2"), "{}", again.body);
    server.stop().await
}

/// Where each of `needles` first appears in `body`, in order.
fn positions(body: &str, needles: &[&str]) -> TestResult<Vec<usize>> {
    needles
        .iter()
        .map(|n| {
            body.find(n)
                .ok_or_else(|| format!("no {n} in the page").into())
        })
        .collect()
}

#[tokio::test]
async fn setup_asks_for_the_new_password_before_the_qr_code_and_the_code() -> TestResult<()> {
    let (server, initial) = start_with_user(|_| {}).await?;
    let signed = server
        .post_public(
            "/admin/sign-in",
            None,
            &[("user", "alice"), ("password", &initial)],
        )
        .await?;
    let ticket = set_cookie(&signed, "joedocs_setup").ok_or("no setup cookie")?;
    let setup = server.get("/admin/setup", Some(&ticket)).await?;
    assert_eq!(setup.status, 200, "{}", setup.body);
    let at = positions(
        &setup.body,
        &[
            "First choose your password",
            "name=\"password\"",
            "name=\"password_again\"",
            "class=\"qr\"",
            "id=\"totp-secret\"",
            "name=\"code\"",
            "type=\"submit\"",
        ],
    )?;
    assert!(at.windows(2).all(|w| w[0] < w[1]), "{at:?}: {}", setup.body);
    assert_eq!(html_problems(&setup.body), Vec::<String>::new());

    // After a password reset the page asks only for the password.
    let secret = shown_secret(&setup.body).ok_or("no secret")?;
    server
        .post_public(
            "/admin/setup",
            Some(&ticket),
            &[
                ("code", &code_for(&server, &secret)),
                ("password", USER_PASSWORD),
                ("password_again", USER_PASSWORD),
            ],
        )
        .await?;
    let reset = joedocs_cli(server.dir.path(), &["user", "reset-password", "alice"])?;
    let again = reset.lines().last().ok_or("no one-time password")?;
    server.clock.advance(30);
    let signed = server
        .post_public(
            "/admin/sign-in",
            None,
            &[
                ("user", "alice"),
                ("password", again),
                ("code", &code_for(&server, &secret)),
            ],
        )
        .await?;
    let ticket = set_cookie(&signed, "joedocs_setup").ok_or("no setup cookie")?;
    let page = server.get("/admin/setup", Some(&ticket)).await?;
    assert!(
        page.body.contains("name=\"password_again\""),
        "{}",
        page.body
    );
    assert!(!page.body.contains("class=\"qr\""), "{}", page.body);
    assert!(!page.body.contains("name=\"code\""), "{}", page.body);
    server.stop().await
}
