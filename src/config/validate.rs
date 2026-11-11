//! Rules a typed configuration must satisfy beyond its types.

use std::collections::BTreeSet;
use std::path::Path;

use super::Config;
use crate::document::is_slug;

/// One broken rule, naming the dotted key at fault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// Dotted key, e.g. `documents.default_type`.
    pub key: &'static str,
    /// What is wrong with its value.
    pub message: String,
}

impl Violation {
    fn new(key: &'static str, message: impl Into<String>) -> Self {
        Self {
            key,
            message: message.into(),
        }
    }
}

impl Config {
    /// Checks the rules that types alone cannot express. The first broken
    /// rule is returned.
    ///
    /// # Errors
    ///
    /// Returns the [`Violation`] for the first key that breaks a rule.
    pub fn validate(&self) -> Result<(), Violation> {
        if self.server.allowed_hosts.is_empty() {
            return Err(Violation::new(
                "server.allowed_hosts",
                "must not be empty; list the host names clients use, such as localhost",
            ));
        }
        for host in &self.server.allowed_hosts {
            if host.is_empty() || host.contains(['/', ' ', '?', '#', '@']) {
                return Err(Violation::new(
                    "server.allowed_hosts",
                    format!(
                        "\"{host}\" is not a host name like docs.example.net or localhost:8750"
                    ),
                ));
            }
        }
        for origin in &self.server.allowed_origins {
            if !is_origin(origin) {
                return Err(Violation::new(
                    "server.allowed_origins",
                    format!("\"{origin}\" is not an origin like https://docs.example.net"),
                ));
            }
        }
        if !self.client.server.is_empty() && !is_server_url(&self.client.server) {
            return Err(Violation::new(
                "client.server",
                format!(
                    "\"{}\" must be an https:// URL like https://docs.example.net (or http:// to localhost, 127.0.0.1 or [::1])",
                    self.client.server
                ),
            ));
        }
        if !(1..=168).contains(&self.admin.session_hours) {
            return Err(Violation::new(
                "admin.session_hours",
                "must be from 1 to 168 (a week)",
            ));
        }
        if self.admin.web_ui && !self.admin.enabled {
            return Err(Violation::new(
                "admin.web_ui",
                "needs admin.enabled = true: the web interface is part of the admin API",
            ));
        }
        if !self.admin.web_plantuml_url.is_empty() && !is_service_url(&self.admin.web_plantuml_url)
        {
            return Err(Violation::new(
                "admin.web_plantuml_url",
                format!(
                    "\"{}\" must be an https:// URL like https://www.plantuml.com/plantuml (or http:// to localhost, 127.0.0.1 or [::1]), with no query or fragment",
                    self.admin.web_plantuml_url
                ),
            ));
        }
        if self.documents.max_files == 0 {
            return Err(Violation::new(
                "documents.max_files",
                "must be greater than 0",
            ));
        }
        if self.server.max_document_bytes == 0 {
            return Err(Violation::new(
                "server.max_document_bytes",
                "must be greater than 0",
            ));
        }
        absolute_or_empty("storage.home", &self.storage.home)?;
        absolute_or_empty("storage.state_dir", &self.storage.state_dir)?;

        let docs = &self.documents;
        slug_list("documents.types", &docs.types)?;
        if docs.types.iter().any(|t| t == "all") {
            return Err(Violation::new(
                "documents.types",
                "\"all\" is reserved for listing every type",
            ));
        }
        slug_list("documents.statuses", &docs.statuses)?;
        member(
            "documents.default_type",
            &docs.default_type,
            "documents.types",
            &docs.types,
        )?;
        member(
            "documents.default_status",
            &docs.default_status,
            "documents.statuses",
            &docs.statuses,
        )?;
        Ok(())
    }
}

/// `scheme://host[:port]` with an http(s) scheme and no path.
fn is_origin(value: &str) -> bool {
    let rest = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"));
    rest.is_some_and(|host| !host.is_empty() && !host.contains(['/', '?', '#', ' ']))
}

fn absolute_or_empty(key: &'static str, path: &Path) -> Result<(), Violation> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        Ok(())
    } else {
        Err(Violation::new(
            key,
            format!("\"{}\" must be an absolute path", path.display()),
        ))
    }
}

fn slug_list(key: &'static str, values: &[String]) -> Result<(), Violation> {
    if values.is_empty() {
        return Err(Violation::new(key, "must not be empty"));
    }
    let mut seen = BTreeSet::new();
    for value in values {
        if !is_slug(value) {
            return Err(Violation::new(
                key,
                format!(
                    "\"{value}\" must be lower-case letters, digits and '-', starting with a letter or digit"
                ),
            ));
        }
        if !seen.insert(value) {
            return Err(Violation::new(key, format!("\"{value}\" is listed twice")));
        }
    }
    Ok(())
}

fn member(
    key: &'static str,
    value: &str,
    list_key: &str,
    list: &[String],
) -> Result<(), Violation> {
    if list.iter().any(|item| item == value) {
        Ok(())
    } else {
        Err(Violation::new(
            key,
            format!("\"{value}\" is not one of {list_key}"),
        ))
    }
}

/// `https://host[:port][/]`, or `http://` to a loopback host.
/// Whether `url` is a usable `client.server`: `https://host[:port]`, or
/// `http://` to `localhost`, `127.0.0.1` or `[::1]`, with no path.
pub(crate) fn is_server_url(url: &str) -> bool {
    let (secure, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else {
        return false;
    };
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.is_empty() || authority.contains(['/', '?', '#', '@', ' ']) {
        return false;
    }
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        let Some((host, after)) = bracketed.split_once(']') else {
            return false;
        };
        if !(after.is_empty() || after.strip_prefix(':').is_some_and(is_port)) {
            return false;
        }
        host
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) if is_port(port) => host,
            Some(_) => return false,
            None => authority,
        }
    };
    !host.is_empty() && (secure || matches!(host, "localhost" | "127.0.0.1" | "::1"))
}

/// Whether `url` is a usable service base URL: what [`is_server_url`]
/// accepts, optionally followed by a path, with no query or fragment.
fn is_service_url(url: &str) -> bool {
    let Some(scheme_end) = url.find("://") else {
        return false;
    };
    let after = scheme_end + 3;
    let (base, path) = match url[after..].find('/') {
        Some(slash) => url.split_at(after + slash),
        None => (url, ""),
    };
    is_server_url(base) && !path.contains(['?', '#', ' ', '@'])
}

fn is_port(port: &str) -> bool {
    !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn violation(edit: impl FnOnce(&mut Config)) -> Violation {
        let mut config = Config::default();
        edit(&mut config);
        config.validate().unwrap_err()
    }

    #[test]
    fn origins_must_be_scheme_and_host_only() {
        let mut config = Config::default();
        config.server.allowed_origins = vec![
            "https://docs.example.net".into(),
            "http://localhost:8750".into(),
        ];
        assert_eq!(config.validate(), Ok(()));

        for bad in [
            "docs.example.net",
            "https://",
            "https://a/b",
            "ftp://a",
            "https://a b",
        ] {
            let v = violation(|c| c.server.allowed_origins = vec![bad.into()]);
            assert_eq!(v.key, "server.allowed_origins", "{bad}");
        }
    }

    #[test]
    fn allowed_hosts_must_be_bare_host_names() {
        let v = violation(|c| c.server.allowed_hosts.clear());
        assert_eq!(v.key, "server.allowed_hosts");
        assert!(v.message.contains("must not be empty"), "{}", v.message);
        for bad in ["", "https://x", "x/y", "a b"] {
            let v = violation(|c| c.server.allowed_hosts = vec![bad.into()]);
            assert_eq!(v.key, "server.allowed_hosts", "{bad}");
        }
        let mut config = Config::default();
        config.server.allowed_hosts = vec!["docs.example.net".into(), "localhost:8750".into()];
        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn client_servers_are_https_or_loopback_http() {
        for good in [
            "https://docs.example.net",
            "https://docs.example.net/",
            "https://docs.example.net:8443",
            "http://localhost:8750",
            "http://127.0.0.1:8750",
            "http://[::1]:8750",
            "https://[2001:db8::1]",
        ] {
            assert!(is_server_url(good), "{good}");
        }
        for bad in [
            "http://example.com",
            "https://",
            "https://host/path",
            "https://user@host",
            "ftp://host",
            "https://host:port",
            "http://[::1",
            "docs.example.net",
        ] {
            assert!(!is_server_url(bad), "{bad}");
        }
        let v = violation(|c| c.client.server = "http://example.com".into());
        assert_eq!(v.key, "client.server");
    }

    #[test]
    fn the_web_interface_needs_the_admin_api() {
        let v = violation(|c| c.admin.web_ui = true);
        assert_eq!(v.key, "admin.web_ui");
        assert!(v.message.contains("admin.enabled"), "{}", v.message);
        let mut config = Config::default();
        config.admin.enabled = true;
        config.admin.web_ui = true;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn plantuml_urls_are_https_or_loopback_http_with_an_optional_path() {
        for good in [
            "",
            "https://plantuml.example.net",
            "https://plantuml.example.net/",
            "https://plantuml.example.net:8443/plantuml",
            "https://www.plantuml.com/plantuml",
            "http://localhost:8080",
            "http://127.0.0.1:8080/plantuml/",
            "http://[::1]:8080",
        ] {
            let mut config = Config::default();
            config.admin.web_plantuml_url = good.into();
            assert!(config.validate().is_ok(), "{good}");
        }
        for bad in [
            "http://plantuml.example.net",
            "https://",
            "https:///plantuml",
            "https://user@plantuml.example.net",
            "https://plantuml.example.net/plantuml?x=1",
            "https://plantuml.example.net/plantuml#top",
            "https://plantuml.example.net/a b",
            "ftp://plantuml.example.net",
            "plantuml.example.net",
        ] {
            let v = violation(|c| c.admin.web_plantuml_url = bad.into());
            assert_eq!(v.key, "admin.web_plantuml_url", "{bad}");
        }
    }

    #[test]
    fn admin_sessions_last_one_hour_to_a_week() {
        for hours in [0, 169] {
            let v = violation(|c| c.admin.session_hours = hours);
            assert_eq!(v.key, "admin.session_hours");
        }
        let mut config = Config::default();
        config.admin.session_hours = 168;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn max_files_must_be_positive() {
        let v = violation(|c| c.documents.max_files = 0);
        assert_eq!(v.key, "documents.max_files");
        let mut config = Config::default();
        config.documents.max_files = 1;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn max_document_bytes_must_be_positive() {
        let v = violation(|c| c.server.max_document_bytes = 0);
        assert_eq!(
            v,
            Violation::new("server.max_document_bytes", "must be greater than 0")
        );
    }

    #[test]
    fn storage_paths_must_be_absolute_when_set() {
        let v = violation(|c| c.storage.home = PathBuf::from("docs"));
        assert_eq!(v.key, "storage.home");
        assert_eq!(v.message, "\"docs\" must be an absolute path");
        let v = violation(|c| c.storage.state_dir = PathBuf::from("./state"));
        assert_eq!(v.key, "storage.state_dir");

        let mut config = Config::default();
        config.storage.home = PathBuf::from("/srv/joedocs");
        config.storage.state_dir = PathBuf::from("/var/lib/joedocs");
        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn types_and_statuses_must_be_unique_non_empty_slugs() {
        let v = violation(|c| c.documents.types.clear());
        assert_eq!(v, Violation::new("documents.types", "must not be empty"));
        let v = violation(|c| c.documents.types.push("Plans".into()));
        assert_eq!(v.key, "documents.types");
        assert!(v.message.contains("\"Plans\""), "{}", v.message);
        let v = violation(|c| c.documents.types.push("home".into()));
        assert_eq!(v.message, "\"home\" is listed twice");
        let v = violation(|c| c.documents.types.push("all".into()));
        assert!(v.message.contains("reserved"), "{}", v.message);
        let v = violation(|c| c.documents.statuses = vec!["in review".into()]);
        assert_eq!(v.key, "documents.statuses");
    }

    #[test]
    fn defaults_must_be_listed() {
        let v = violation(|c| c.documents.default_type = "memo".into());
        assert_eq!(
            v,
            Violation::new(
                "documents.default_type",
                "\"memo\" is not one of documents.types"
            )
        );
        let v = violation(|c| c.documents.default_status = "open".into());
        assert_eq!(v.key, "documents.default_status");
    }
}
