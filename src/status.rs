//! `joedocs status`: where the service runs, how much the vault and state
//! directory hold, and the state of the store. Changes nothing and never
//! shows a secret.
//!
//! [`StatusReport`] is plain data, so the admin API can send it to a client,
//! which renders it with the same [`render`].

use std::fmt::Write as _;
use std::io::{self, Read as _, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::store::{Store, Summary};
use crate::systemd::{ServiceManager, UNIT_NAME};
use crate::{Error, Loaded};

/// How long the `/health` probe waits to connect, and then for an answer.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Everything `joedocs status` reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusReport {
    /// The program version.
    pub version: String,
    /// This host's name, or `unknown`.
    pub host: String,
    /// The config file in use, if any.
    pub config_file: Option<PathBuf>,
    /// `server.bind`.
    pub bind: SocketAddr,
    /// The `/health` probe on the bind address.
    pub health: Health,
    /// The systemd user unit.
    pub user_unit: UnitState,
    /// The systemd system unit.
    pub system_unit: UnitState,
    /// `storage.home`.
    pub vault: DirSize,
    /// `storage.state_dir`.
    pub state: DirSize,
    /// The database file with its WAL and shared-memory files, in bytes.
    pub database_bytes: u64,
    /// The history repository.
    pub history: DirSize,
    /// What the store holds.
    pub store: Summary,
}

/// The result of probing `GET /health`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Health {
    /// It answered `ok`.
    Ok,
    /// It answered, but not with `200` and `ok`.
    Unexpected(String),
    /// Nothing answered.
    Unreachable(String),
}

/// The state of a systemd unit named [`UNIT_NAME`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnitState {
    /// `systemctl` could not be run (no systemd, for example on macOS).
    Unavailable,
    /// No such unit is installed.
    NotInstalled,
    /// The unit exists.
    Installed {
        /// `ActiveState`, e.g. `active` or `inactive`.
        active: String,
        /// `MainPID`, when it runs.
        pid: Option<u32>,
    },
}

/// A directory's location and size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirSize {
    /// The directory.
    pub path: PathBuf,
    /// Total size of its regular files, in bytes.
    pub bytes: u64,
    /// Number of regular files (symlinks are not followed or counted).
    pub files: u64,
}

/// Gathers the report.
///
/// # Errors
///
/// Returns storage errors.
pub fn collect(
    store: &Store,
    loaded: &Loaded,
    services: &dyn ServiceManager,
) -> Result<StatusReport, Error> {
    let paths = store.paths();
    let bind = loaded.config().server.bind;
    let database_bytes = ["joedocs.db", "joedocs.db-wal", "joedocs.db-shm"]
        .iter()
        .filter_map(|name| std::fs::metadata(paths.state_dir.join(name)).ok())
        .map(|m| m.len())
        .sum();
    Ok(StatusReport {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        host: services
            .host_name()
            .ok()
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| "unknown".to_owned()),
        config_file: loaded.resolved.file.clone(),
        bind,
        health: probe(bind),
        user_unit: unit_state(services, true),
        system_unit: unit_state(services, false),
        vault: dir_size(&paths.home),
        state: dir_size(&paths.state_dir),
        database_bytes,
        history: dir_size(&paths.git_dir()),
        store: store.summary()?,
    })
}

/// Probes `GET /health` on `bind`, using loopback for a wildcard address.
#[must_use]
pub fn probe(bind: SocketAddr) -> Health {
    let target = match bind.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => {
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), bind.port())
        }
        IpAddr::V6(ip) if ip.is_unspecified() => {
            SocketAddr::new(Ipv6Addr::LOCALHOST.into(), bind.port())
        }
        _ => bind,
    };
    match ask_health(target) {
        Ok(response) => {
            let ok_status =
                response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200");
            if ok_status && response.trim_end().ends_with("ok") {
                Health::Ok
            } else {
                let first = response.lines().next().unwrap_or("an empty response");
                Health::Unexpected(first.to_owned())
            }
        }
        Err(error) => Health::Unreachable(error.to_string()),
    }
}

fn ask_health(target: SocketAddr) -> io::Result<String> {
    let mut stream = TcpStream::connect_timeout(&target, PROBE_TIMEOUT)?;
    stream.set_read_timeout(Some(PROBE_TIMEOUT))?;
    stream.set_write_timeout(Some(PROBE_TIMEOUT))?;
    let host = if target.is_ipv6() {
        "[::1]"
    } else {
        "127.0.0.1"
    };
    let request = format!("GET /health HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes())?;
    let mut response = Vec::new();
    stream.take(64 * 1024).read_to_end(&mut response)?;
    Ok(String::from_utf8_lossy(&response).into_owned())
}

/// The state of the unit in the user (`user`) or system manager.
pub(crate) fn unit_state(services: &dyn ServiceManager, user: bool) -> UnitState {
    let mut args = Vec::new();
    if user {
        args.push("--user");
    }
    args.extend([
        "show",
        UNIT_NAME,
        "--property=LoadState,ActiveState,MainPID",
    ]);
    match services.systemctl(&args) {
        Ok(ran) if ran.success => parse_show(&ran.stdout),
        _ => UnitState::Unavailable,
    }
}

/// Parses `systemctl show --property=LoadState,ActiveState,MainPID`.
fn parse_show(text: &str) -> UnitState {
    let value = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .map(str::trim)
    };
    match value("LoadState") {
        Some("loaded") => UnitState::Installed {
            active: value("ActiveState").unwrap_or("unknown").to_owned(),
            pid: value("MainPID")
                .and_then(|p| p.parse().ok())
                .filter(|p| *p != 0),
        },
        Some(_) => UnitState::NotInstalled,
        None => UnitState::Unavailable,
    }
}

/// The size and file count under `dir`; a missing directory is empty.
fn dir_size(dir: &Path) -> DirSize {
    let mut size = DirSize {
        path: dir.to_path_buf(),
        bytes: 0,
        files: 0,
    };
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.is_dir() {
                pending.push(entry.path());
            } else if meta.is_file() {
                size.bytes += meta.len();
                size.files += 1;
            }
        }
    }
    size
}

/// `1536` → `1.5 KiB`.
pub(crate) fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KiB", "MiB", "GiB", "TiB"];
    #[expect(clippy::cast_precision_loss, reason = "a rounded display value")]
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} bytes")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn describe_dir(size: &DirSize) -> String {
    format!(
        "{} ({}, {} bytes, {} files)",
        size.path.display(),
        human(size.bytes),
        size.bytes,
        size.files
    )
}

fn describe_unit(state: &UnitState) -> String {
    match state {
        UnitState::Unavailable => "systemctl not available".to_owned(),
        UnitState::NotInstalled => "not installed".to_owned(),
        UnitState::Installed {
            active,
            pid: Some(pid),
        } => format!("{active}, MainPID {pid}"),
        UnitState::Installed { active, pid: None } => active.clone(),
    }
}

/// Writes the report as text.
///
/// # Errors
///
/// Returns [`Error::Output`] when writing fails.
pub fn render(report: &StatusReport, out: &mut dyn Write) -> Result<(), Error> {
    let mut text = String::new();
    let _ = writeln!(text, "JoeDocs {} on {}", report.version, report.host);
    let config = report.config_file.as_ref().map_or_else(
        || "none (built-in defaults)".to_owned(),
        |f| f.display().to_string(),
    );
    let _ = writeln!(text, "config file: {config}");
    let _ = writeln!(text, "\nService");
    let _ = writeln!(text, "  bind: {}", report.bind);
    let health = match &report.health {
        Health::Ok => "ok".to_owned(),
        Health::Unexpected(first) => format!("unexpected answer ({first})"),
        Health::Unreachable(error) => format!("not reachable ({error})"),
    };
    let _ = writeln!(text, "  health: {health}");
    let _ = writeln!(
        text,
        "  systemd user unit {UNIT_NAME}: {}",
        describe_unit(&report.user_unit)
    );
    let _ = writeln!(
        text,
        "  systemd system unit {UNIT_NAME}: {}",
        describe_unit(&report.system_unit)
    );
    let _ = writeln!(text, "\nStorage");
    let _ = writeln!(text, "  vault: {}", describe_dir(&report.vault));
    let _ = writeln!(text, "  state: {}", describe_dir(&report.state));
    let _ = writeln!(
        text,
        "  database: {} ({} bytes)",
        human(report.database_bytes),
        report.database_bytes
    );
    let _ = writeln!(
        text,
        "  history: {} ({} files)",
        human(report.history.bytes),
        report.history.files
    );
    render_store(&mut text, &report.store);
    out.write_all(text.as_bytes())?;
    Ok(())
}

/// The store and project sections of the report.
fn render_store(text: &mut String, store: &Summary) {
    let _ = writeln!(*text, "\nStore");
    let head = store
        .history_head
        .as_deref()
        .unwrap_or("none (no commits yet)");
    let _ = writeln!(*text, "  history head: {head}");
    if let Some(at) = &store.last_change {
        let _ = writeln!(*text, "  last change: {at}");
    }
    let index = if store.index_current {
        "current"
    } else {
        "behind the history (`serve` or `joedocs reindex` rebuilds it)"
    };
    let _ = writeln!(*text, "  index: {index}");
    let stale = store.stale_index_files.len();
    let index_files = if stale == 0 {
        "up to date".to_owned()
    } else {
        format!("{stale} missing or out of date (`joedocs reindex` rewrites them)")
    };
    let _ = writeln!(*text, "  INDEX.md files: {index_files}");
    let _ = writeln!(
        *text,
        "  API keys: {} active, {} revoked",
        store.keys_active, store.keys_revoked
    );
    let _ = writeln!(
        *text,
        "  admin tokens: {} active, {} revoked",
        store.admin_tokens_active, store.admin_tokens_revoked
    );
    let _ = writeln!(*text, "\nProjects");
    if store.projects.is_empty() {
        let _ = writeln!(*text, "  none (add one with `joedocs project add`)");
    } else {
        let width = store
            .projects
            .iter()
            .map(|p| p.name.len())
            .max()
            .unwrap_or(4)
            .max(4);
        let key_width = store
            .projects
            .iter()
            .map(|p| p.key.len())
            .max()
            .unwrap_or(3)
            .max(3);
        let _ = writeln!(
            text,
            "  {:width$}  {:key_width$}  {:>5}  {:>8}",
            "NAME", "KEY", "LIVE", "ARCHIVED"
        );
        for p in &store.projects {
            let _ = writeln!(
                *text,
                "  {:width$}  {:key_width$}  {:>5}  {:>8}",
                p.name, p.key, p.live, p.archived
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemctl_show_output_is_parsed() {
        assert_eq!(
            parse_show("MainPID=4242\nLoadState=loaded\nActiveState=active\n"),
            UnitState::Installed {
                active: "active".into(),
                pid: Some(4242)
            }
        );
        assert_eq!(
            parse_show("LoadState=loaded\nActiveState=inactive\nMainPID=0\n"),
            UnitState::Installed {
                active: "inactive".into(),
                pid: None
            }
        );
        assert_eq!(
            parse_show("LoadState=not-found\nActiveState=inactive\n"),
            UnitState::NotInstalled
        );
        assert_eq!(parse_show(""), UnitState::Unavailable);
    }

    struct Show(io::Result<crate::systemd::Ran>);

    impl ServiceManager for Show {
        fn executable(&self) -> io::Result<PathBuf> {
            Ok("/bin/joedocs".into())
        }
        fn systemctl(&self, args: &[&str]) -> io::Result<crate::systemd::Ran> {
            assert!(args.contains(&"show"), "{args:?}");
            match &self.0 {
                Ok(ran) => Ok(ran.clone()),
                Err(error) => Err(io::Error::new(error.kind(), error.to_string())),
            }
        }
        fn host_name(&self) -> io::Result<String> {
            Err(io::Error::other("no host name"))
        }
    }

    #[test]
    fn unit_state_handles_missing_systemctl_and_failures() {
        let missing = Show(Err(io::Error::new(io::ErrorKind::NotFound, "no systemctl")));
        assert_eq!(unit_state(&missing, true), UnitState::Unavailable);
        let failed = Show(Ok(crate::systemd::Ran {
            success: false,
            stdout: String::new(),
            stderr: "Failed to connect to bus".into(),
        }));
        assert_eq!(unit_state(&failed, true), UnitState::Unavailable);
        let active = Show(Ok(crate::systemd::Ran {
            success: true,
            stdout: "LoadState=loaded\nActiveState=active\nMainPID=7\n".into(),
            stderr: String::new(),
        }));
        assert_eq!(
            unit_state(&active, false),
            UnitState::Installed {
                active: "active".into(),
                pid: Some(7)
            }
        );
    }

    /// Reads up to the end of the request headers.
    fn read_request(stream: &mut TcpStream) {
        let mut seen = Vec::new();
        let mut byte = [0_u8; 1];
        while !seen.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
            seen.push(byte[0]);
        }
    }

    #[test]
    fn probe_reports_ok_unexpected_and_unreachable() {
        let answer = |reply: &'static [u8]| {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                read_request(&mut stream);
                stream.write_all(reply).unwrap();
            });
            addr
        };
        let ok = answer(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok");
        assert_eq!(probe(ok), Health::Ok);
        let bad = answer(b"HTTP/1.1 404 Not Found\r\n\r\n");
        assert_eq!(
            probe(bad),
            Health::Unexpected("HTTP/1.1 404 Not Found".into())
        );
        let wildcard = answer(b"HTTP/1.1 200 OK\r\n\r\nok");
        let any = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), wildcard.port());
        assert_eq!(probe(any), Health::Ok);
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = closed.local_addr().unwrap();
        drop(closed);
        assert!(matches!(probe(addr), Health::Unreachable(_)));
    }

    #[test]
    fn sizes_are_human_readable_and_count_regular_files() {
        assert_eq!(human(0), "0 bytes");
        assert_eq!(human(1023), "1023 bytes");
        assert_eq!(human(1536), "1.5 KiB");
        assert_eq!(human(5 * 1024 * 1024), "5.0 MiB");
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("a"), "12345").unwrap();
        std::fs::write(dir.path().join("sub/b"), "123").unwrap();
        std::os::unix::fs::symlink(dir.path().join("a"), dir.path().join("link")).unwrap();
        let size = dir_size(dir.path());
        assert_eq!((size.bytes, size.files), (8, 2));
        let missing = dir_size(&dir.path().join("nope"));
        assert_eq!((missing.bytes, missing.files), (0, 0));
    }

    #[test]
    fn render_describes_units_and_problems() {
        let report = StatusReport {
            version: "9.9.9".into(),
            host: "box".into(),
            config_file: None,
            bind: "127.0.0.1:8750".parse().unwrap(),
            health: Health::Unexpected("HTTP/1.1 500".into()),
            user_unit: UnitState::Installed {
                active: "active".into(),
                pid: Some(12),
            },
            system_unit: UnitState::NotInstalled,
            vault: DirSize {
                path: "/v".into(),
                bytes: 2048,
                files: 3,
            },
            state: DirSize {
                path: "/s".into(),
                bytes: 10,
                files: 1,
            },
            database_bytes: 10,
            history: DirSize {
                path: "/s/history.git".into(),
                bytes: 0,
                files: 0,
            },
            store: Summary {
                projects: Vec::new(),
                keys_active: 0,
                keys_revoked: 2,
                admin_tokens_active: 1,
                admin_tokens_revoked: 0,
                history_head: None,
                last_change: None,
                index_current: false,
                stale_index_files: vec!["p/INDEX.md".into()],
            },
        };
        let mut out = Vec::new();
        render(&report, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        for expected in [
            "JoeDocs 9.9.9 on box",
            "config file: none (built-in defaults)",
            "health: unexpected answer (HTTP/1.1 500)",
            "user unit joedocs.service: active, MainPID 12",
            "system unit joedocs.service: not installed",
            "vault: /v (2.0 KiB, 2048 bytes, 3 files)",
            "history head: none (no commits yet)",
            "index: behind the history",
            "INDEX.md files: 1 missing or out of date",
            "API keys: 0 active, 2 revoked",
            "admin tokens: 1 active, 0 revoked",
            "none (add one with `joedocs project add`)",
        ] {
            assert!(
                text.contains(expected),
                "{expected:?} missing from:\n{text}"
            );
        }
        assert_eq!(
            describe_unit(&UnitState::Installed {
                active: "inactive".into(),
                pid: None
            }),
            "inactive"
        );
        assert_eq!(
            describe_unit(&UnitState::Unavailable),
            "systemctl not available"
        );
    }
}
