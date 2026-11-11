//! MCP server: tool schemas and handlers, the Streamable HTTP transport,
//! bearer API key and `Host`/`Origin` checks, and mapping of errors to MCP
//! results.
//!
//! `POST/GET/DELETE /mcp` is the MCP endpoint and needs
//! `Authorization: Bearer jd_...`. `GET /health` answers `ok` without
//! authentication, for proxies and service managers. See `docs/plan.md`
//! sections 3 and 8.
//!
//! All store access goes through one mutex on a blocking thread, so the
//! store keeps a single writer and SQLite and git never block the async
//! runtime.

mod admin_api;
mod auth;
mod errors;
pub(crate) mod tools;

use std::future::Future;
use std::io::{self, Write};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::routing::get;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

pub use admin_api::{AdminOptions, BODY_LIMIT, SESSION_HEADER};
pub(crate) use admin_api::{classify, host_allowed, strip_port};
pub use auth::Caller;
pub use errors::ToolError;
pub use tools::JoeDocsServer;

use crate::config::Config;
use crate::store::{Settings, Store};

/// Extra request body allowance over `4 × max_document_bytes`, for the
/// JSON-RPC envelope and other arguments.
const BODY_SLACK_BYTES: u64 = 64 * 1024;

/// HTTP settings taken from configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerOptions {
    /// Listen address.
    pub bind: SocketAddr,
    /// Accepted `Host` values.
    pub allowed_hosts: Vec<String>,
    /// Accepted `Origin` values, with explicit ports.
    pub allowed_origins: Vec<String>,
    /// Largest request body. JSON escaping can grow a document up to
    /// several times, so this is `4 × max_document_bytes` plus slack; the
    /// store still enforces `max_document_bytes` on the result.
    pub max_request_bytes: usize,
    /// The admin API, when `admin.enabled` is on.
    pub admin: Option<AdminOptions>,
    /// The admin web interface, when `admin.enabled` and `admin.web_ui`
    /// are both on.
    pub web: Option<crate::web::WebOptions>,
}

impl ServerOptions {
    /// Takes the HTTP settings from validated configuration.
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        let max = config
            .server
            .max_document_bytes
            .saturating_mul(4)
            .saturating_add(BODY_SLACK_BYTES);
        Self {
            bind: config.server.bind,
            allowed_hosts: config.server.allowed_hosts.clone(),
            allowed_origins: config
                .server
                .allowed_origins
                .iter()
                .map(|o| with_default_port(o))
                .collect(),
            max_request_bytes: usize::try_from(max).unwrap_or(usize::MAX),
            admin: config.admin.enabled.then_some(AdminOptions {
                session_hours: config.admin.session_hours,
                config_file: None,
            }),
            web: (config.admin.enabled && config.admin.web_ui).then(|| crate::web::WebOptions {
                session_hours: config.admin.session_hours,
                plantuml_url: config.admin.web_plantuml_url.clone(),
            }),
        }
    }
}

/// `https://host` → `https://host:443`, `http://host` → `http://host:80`, so
/// an origin entry matches only the scheme's default port.
pub(crate) fn with_default_port(origin: &str) -> String {
    let (scheme, rest, port) = if let Some(rest) = origin.strip_prefix("https://") {
        ("https", rest, 443)
    } else if let Some(rest) = origin.strip_prefix("http://") {
        ("http", rest, 80)
    } else {
        return origin.to_owned();
    };
    let has_port = if let Some(after_bracket) = rest.rsplit_once(']') {
        after_bracket.1.starts_with(':')
    } else {
        rest.contains(':')
    };
    if has_port {
        origin.to_owned()
    } else {
        format!("{scheme}://{rest}:{port}")
    }
}

/// State shared by every request.
pub struct Shared {
    store: Mutex<Store>,
    settings: Settings,
}

impl Shared {
    /// Wraps a store for concurrent use.
    #[must_use]
    pub fn new(store: Store) -> Arc<Self> {
        let settings = store.settings().clone();
        Arc::new(Self {
            store: Mutex::new(store),
            settings,
        })
    }

    /// The store's lock, for the web interface's handlers, which run on
    /// blocking threads.
    pub(crate) fn store(&self) -> &Mutex<Store> {
        &self.store
    }

    /// Runs `f` with exclusive access to the store on a blocking thread.
    async fn with_store<T, F>(self: &Arc<Self>, f: F) -> Result<T, ToolError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Store) -> Result<T, ToolError> + Send + 'static,
    {
        let shared = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let mut store = shared
                .store
                .lock()
                .map_err(|_| ToolError::internal("the store lock is poisoned"))?;
            f(&mut store)
        })
        .await
        .map_err(|error| ToolError::internal(format!("store task failed: {error}")))?
    }
}

/// The HTTP application: `/mcp` behind bearer authentication, and
/// `/health`. Cancelling `ct` ends every MCP session.
pub fn router(shared: &Arc<Shared>, options: &ServerOptions, ct: &CancellationToken) -> Router {
    let config = StreamableHttpServerConfig::default()
        .with_allowed_hosts(options.allowed_hosts.clone())
        .with_allowed_origins(options.allowed_origins.clone())
        .enforce_origin_validation()
        .with_max_request_body_bytes(options.max_request_bytes)
        .with_cancellation_token(ct.child_token());
    let for_sessions = Arc::clone(shared);
    let service = StreamableHttpService::new(
        move || Ok(JoeDocsServer::new(Arc::clone(&for_sessions))),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    let mcp =
        Router::new()
            .nest_service("/mcp", service)
            .layer(axum::middleware::from_fn_with_state(
                Arc::clone(shared),
                auth::require_key,
            ));
    let mut app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .merge(mcp);
    if let Some(web) = &options.web {
        app = app.merge(crate::web::routes(shared, web, options));
    }
    match &options.admin {
        Some(admin) => app.nest("/admin/v1", admin_api::routes(shared, admin, options)),
        // Without this, the MCP router's key check would answer these paths
        // with 401, as for any other unknown path; a disabled admin API is
        // simply not there.
        None => app
            .route("/admin/v1", axum::routing::any(not_found))
            .route("/admin/v1/{*rest}", axum::routing::any(not_found)),
    }
}

async fn not_found() -> axum::http::StatusCode {
    axum::http::StatusCode::NOT_FOUND
}

/// Serves on `listener` until `shutdown` completes, then closes every MCP
/// session and waits for open requests to finish.
///
/// # Errors
///
/// Returns an error when the server fails.
pub async fn serve(
    listener: TcpListener,
    shared: Arc<Shared>,
    options: &ServerOptions,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    let ct = CancellationToken::new();
    let app = router(&shared, options, &ct);
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.await;
            tracing::info!("shutting down");
            ct.cancel();
        })
        .await
}

/// Runs the server for `joedocs serve`: binds `server.bind`, prints the
/// endpoint to `out`, and serves until Ctrl-C or `SIGTERM`.
///
/// # Errors
///
/// Returns [`crate::Error::Serve`] when the runtime cannot start, the
/// address cannot be bound, or the server fails, and
/// [`crate::Error::Output`] when writing to `out` fails.
pub fn run(
    store: Store,
    config: &Config,
    config_file: Option<&std::path::Path>,
    out: &mut dyn Write,
) -> Result<(), crate::Error> {
    let mut options = ServerOptions::from_config(config);
    if let Some(admin) = &mut options.admin {
        admin.config_file = config_file.map(std::path::Path::to_path_buf);
    }
    let addr = options.bind;
    let failed = |source| crate::Error::Serve { addr, source };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(failed)?;
    runtime.block_on(async {
        let listener = TcpListener::bind(addr).await.map_err(failed)?;
        let local = listener.local_addr().map_err(failed)?;
        writeln!(out, "JoeDocs MCP endpoint: http://{local}/mcp")?;
        if options.admin.is_some() {
            writeln!(out, "JoeDocs admin API: http://{local}/admin/v1/")?;
        }
        out.flush()?;
        tracing::info!(%local, "listening");
        serve(listener, Shared::new(store), &options, shutdown_signal())
            .await
            .map_err(failed)
    })
}

/// Completes on Ctrl-C or `SIGTERM`.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::warn!(%error, "cannot listen for Ctrl-C");
            std::future::pending::<()>().await;
        }
    };
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::warn!(%error, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_get_the_scheme_default_port() {
        assert_eq!(with_default_port("https://a.net"), "https://a.net:443");
        assert_eq!(with_default_port("http://a.net"), "http://a.net:80");
        assert_eq!(with_default_port("http://a.net:8080"), "http://a.net:8080");
        assert_eq!(with_default_port("http://[::1]"), "http://[::1]:80");
        assert_eq!(with_default_port("http://[::1]:9"), "http://[::1]:9");
        assert_eq!(with_default_port("null"), "null");
    }

    #[test]
    fn options_come_from_config() {
        let mut config = Config::default();
        config.server.allowed_origins = vec!["https://docs.example.net".into()];
        config.server.max_document_bytes = 10;
        let options = ServerOptions::from_config(&config);
        assert_eq!(options.allowed_origins, ["https://docs.example.net:443"]);
        assert_eq!(options.allowed_hosts, ["localhost", "127.0.0.1", "::1"]);
        assert_eq!(options.max_request_bytes, 40 + 64 * 1024);
        assert_eq!(options.bind, config.server.bind);
    }
}
