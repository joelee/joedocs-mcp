//! The browser session: the cookie that carries a web session token, the
//! CSRF token derived from it, the same-origin rule for `POST`s (JD-32 D-09)
//! and the HTTPS rule for logins (D-10).
//!
//! The cookie is `HttpOnly` and `SameSite=Strict`; over HTTPS it is also
//! `Secure` and named with the `__Host-` prefix, so it is bound to this host
//! and path `/`. It holds the web session token (`jdw_...`) only: the admin
//! token is used once, to log in, and never kept.

use axum::http::{HeaderMap, header};
use hmac::{Hmac, KeyInit as _, Mac as _};
use secrecy::{ExposeSecret as _, SecretString};
use sha2::Sha256;

use crate::mcp::{strip_port, with_default_port};

/// The cookie name over HTTPS.
pub const SECURE_COOKIE: &str = "__Host-joedocs_session";
/// The cookie name over loopback HTTP.
pub const PLAIN_COOKIE: &str = "joedocs_session";

/// What the CSRF token is derived for, so the HMAC of the session secret
/// cannot be confused with any other use of it.
const CSRF_LABEL: &[u8] = b"joedocs-web-csrf-v1";

/// A request header as text, trimmed.
pub fn text<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
}

/// The web session token in the request's cookies, preferring the
/// `__Host-` cookie.
#[must_use]
pub fn session_cookie(headers: &HeaderMap) -> Option<SecretString> {
    let mut plain = None;
    for value in headers.get_all(header::COOKIE) {
        let Ok(value) = value.to_str() else { continue };
        for pair in value.split(';') {
            let Some((name, token)) = pair.trim().split_once('=') else {
                continue;
            };
            if token.is_empty() {
                continue;
            }
            match name {
                SECURE_COOKIE => return Some(SecretString::from(token.to_owned())),
                PLAIN_COOKIE => plain = Some(SecretString::from(token.to_owned())),
                _ => {}
            }
        }
    }
    plain
}

/// The `Set-Cookie` value that stores `session` for `max_age` seconds.
#[must_use]
pub fn set_cookie(session: &SecretString, secure: bool, max_age: u64) -> SecretString {
    let name = if secure { SECURE_COOKIE } else { PLAIN_COOKIE };
    let secure = if secure { "; Secure" } else { "" };
    SecretString::from(format!(
        "{name}={}; HttpOnly; SameSite=Strict; Path=/; Max-Age={max_age}{secure}",
        session.expose_secret()
    ))
}

/// The `Set-Cookie` values that clear both cookie names.
#[must_use]
pub fn clear_cookies() -> [&'static str; 2] {
    [
        "__Host-joedocs_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0; Secure",
        "joedocs_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0",
    ]
}

/// The CSRF token for `session`: HMAC-SHA256 of a fixed label, keyed by the
/// session token, in hex. Forms carry it in a hidden `csrf` field.
#[must_use]
pub fn csrf_token(session: &SecretString) -> String {
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(session.expose_secret().as_bytes()) else {
        // HMAC takes a key of any length; this cannot happen.
        return String::new();
    };
    mac.update(CSRF_LABEL);
    crate::auth::hex(&mac.finalize().into_bytes())
}

/// Whether `sent` is the CSRF token of `session`, compared in constant
/// time.
#[must_use]
pub fn csrf_matches(session: &SecretString, sent: Option<&str>) -> bool {
    let expected = csrf_token(session);
    let Some(sent) = sent else { return false };
    !expected.is_empty()
        && sent.len() == expected.len()
        && sent
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

/// The same-origin rule for a `POST` (D-09): `Sec-Fetch-Site`, when sent, is
/// `same-origin`; and `Origin`, when sent, is this request's own origin (its
/// `Host`, by either scheme) or one of `allowed_origins` (with explicit
/// ports).
#[must_use]
pub fn same_origin(headers: &HeaderMap, allowed_origins: &[String]) -> bool {
    if let Some(site) = text(headers, "sec-fetch-site")
        && !site.eq_ignore_ascii_case("same-origin")
    {
        return false;
    }
    let Some(origin) = text(headers, header::ORIGIN.as_str()) else {
        return true;
    };
    let origin = with_default_port(origin);
    if allowed_origins.contains(&origin) {
        return true;
    }
    let host = text(headers, header::HOST.as_str()).unwrap_or("");
    !host.is_empty()
        && ["http", "https"]
            .iter()
            .any(|scheme| with_default_port(&format!("{scheme}://{host}")) == origin)
}

/// Whether the request reached the server over HTTPS, as the browser sees
/// it: an `https://` `Origin`, or `X-Forwarded-Proto: https` from a proxy.
#[must_use]
pub fn is_https(headers: &HeaderMap) -> bool {
    if let Some(origin) = text(headers, header::ORIGIN.as_str()) {
        return origin.starts_with("https://");
    }
    text(headers, "x-forwarded-proto").is_some_and(|p| p.eq_ignore_ascii_case("https"))
}

/// Whether the request's `Host` is a loopback name.
#[must_use]
pub fn is_loopback(headers: &HeaderMap) -> bool {
    let host = text(headers, header::HOST.as_str()).unwrap_or("");
    matches!(strip_port(host), "localhost" | "127.0.0.1" | "::1")
}

/// `next` if it is a page of the interface, else `/admin/`: a login never
/// sends the browser anywhere else.
#[must_use]
pub fn safe_next(next: Option<&str>) -> String {
    let Some(next) = next else {
        return "/admin/".to_owned();
    };
    let inside = next == "/admin" || next.starts_with("/admin/");
    let tricky = next.contains("//")
        || next.contains('\\')
        || next.contains("/../")
        || next.contains("/./")
        || next.ends_with("/..")
        || next.chars().any(char::is_control);
    if inside && !tricky {
        next.to_owned()
    } else {
        "/admin/".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(*name, HeaderValue::from_static(value));
        }
        map
    }

    #[test]
    fn the_host_cookie_wins_and_empty_cookies_are_ignored() {
        let map = headers(&[(
            "cookie",
            "other=1; joedocs_session=jdw_plain; __Host-joedocs_session=jdw_secure",
        )]);
        assert_eq!(
            session_cookie(&map).map(|s| s.expose_secret().to_owned()),
            Some("jdw_secure".to_owned())
        );
        let map = headers(&[("cookie", "joedocs_session=jdw_plain"), ("cookie", "x=y")]);
        assert_eq!(
            session_cookie(&map).map(|s| s.expose_secret().to_owned()),
            Some("jdw_plain".to_owned())
        );
        assert!(session_cookie(&headers(&[("cookie", "joedocs_session=")])).is_none());
        assert!(session_cookie(&HeaderMap::new()).is_none());
    }

    #[test]
    fn csrf_tokens_are_per_session_and_checked_exactly() {
        let a = SecretString::from("jdw_a".to_owned());
        let b = SecretString::from("jdw_b".to_owned());
        let token = csrf_token(&a);
        assert_eq!(token.len(), 64);
        assert_ne!(token, csrf_token(&b));
        assert!(csrf_matches(&a, Some(&token)));
        assert!(!csrf_matches(&b, Some(&token)));
        assert!(!csrf_matches(&a, Some(&token[1..])));
        assert!(!csrf_matches(&a, None));
    }

    #[test]
    fn the_same_origin_rule() {
        let host = ("host", "docs.example.net");
        assert!(same_origin(&headers(&[host]), &[]));
        assert!(same_origin(
            &headers(&[host, ("origin", "https://docs.example.net")]),
            &[]
        ));
        assert!(same_origin(
            &headers(&[
                ("host", "127.0.0.1:8750"),
                ("origin", "http://127.0.0.1:8750"),
                ("sec-fetch-site", "same-origin"),
            ]),
            &[]
        ));
        assert!(!same_origin(
            &headers(&[host, ("origin", "https://evil.example")]),
            &[]
        ));
        assert!(!same_origin(&headers(&[host, ("origin", "null")]), &[]));
        assert!(!same_origin(
            &headers(&[host, ("sec-fetch-site", "cross-site")]),
            &[]
        ));
        assert!(!same_origin(
            &headers(&[host, ("sec-fetch-site", "same-site")]),
            &[]
        ));
        assert!(same_origin(
            &headers(&[
                ("host", "127.0.0.1:8750"),
                ("origin", "https://proxy.example")
            ]),
            &["https://proxy.example:443".to_owned()]
        ));
    }

    #[test]
    fn https_is_seen_through_the_origin_or_a_proxy_header() {
        assert!(is_https(&headers(&[("origin", "https://a.example")])));
        assert!(!is_https(&headers(&[("origin", "http://a.example")])));
        assert!(is_https(&headers(&[("x-forwarded-proto", "https")])));
        assert!(!is_https(&HeaderMap::new()));
        assert!(is_loopback(&headers(&[("host", "localhost:8750")])));
        assert!(is_loopback(&headers(&[("host", "[::1]:8750")])));
        assert!(!is_loopback(&headers(&[("host", "docs.example.net")])));
    }

    #[test]
    fn next_stays_inside_the_interface() {
        assert_eq!(
            safe_next(Some("/admin/documents?p=x")),
            "/admin/documents?p=x"
        );
        assert_eq!(safe_next(Some("/admin")), "/admin");
        for bad in [
            "https://evil.example/",
            "//evil.example/admin/",
            "/admin/../x",
            "/admin/..",
            "/administrator",
            "/admin/\\evil",
            "/other",
        ] {
            assert_eq!(safe_next(Some(bad)), "/admin/", "{bad}");
        }
        assert_eq!(safe_next(None), "/admin/");
    }
}
