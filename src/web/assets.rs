//! The interface's static files, compiled into the binary. Each is served at
//! `/admin/assets/<hash>/<name>`, where `<hash>` is the first 16 hex digits
//! of its SHA-256, so a file can be cached for a year and a new release
//! still loads its own copy.

use std::sync::LazyLock;

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse as _, Response};
use sha2::{Digest as _, Sha256};

/// One embedded file.
struct Asset {
    name: &'static str,
    content_type: &'static str,
    bytes: &'static [u8],
}

const FILES: &[Asset] = &[
    Asset {
        name: "app.css",
        content_type: "text/css; charset=utf-8",
        bytes: include_bytes!("assets/app.css"),
    },
    Asset {
        name: "app.js",
        content_type: "text/javascript; charset=utf-8",
        bytes: include_bytes!("assets/app.js"),
    },
    Asset {
        name: "htmx.min.js",
        content_type: "text/javascript; charset=utf-8",
        bytes: include_bytes!("assets/vendor/htmx.min.js"),
    },
    Asset {
        name: "pele.global.js",
        content_type: "text/javascript; charset=utf-8",
        bytes: include_bytes!("assets/vendor/pele.global.js"),
    },
];

/// Where each file is served.
static PATHS: LazyLock<Vec<String>> = LazyLock::new(|| {
    FILES
        .iter()
        .map(|asset| {
            let digest = Sha256::digest(asset.bytes);
            let hash = crate::auth::hex(&digest[..8]);
            format!("/admin/assets/{hash}/{}", asset.name)
        })
        .collect()
});

/// The URL of the embedded file `name`, or `""` for an unknown name.
#[must_use]
pub fn url(name: &str) -> &'static str {
    FILES
        .iter()
        .position(|asset| asset.name == name)
        .and_then(|i| PATHS.get(i))
        .map_or("", String::as_str)
}

/// The URLs the base template links.
#[derive(Debug, Clone, Copy)]
pub struct Urls {
    /// `app.css`.
    pub css: &'static str,
    /// `app.js`.
    pub js: &'static str,
    /// `htmx.min.js`.
    pub htmx: &'static str,
    /// `pele.global.js`, loaded only by pages with a Mermaid diagram.
    pub pele: &'static str,
}

/// The URLs of the files every page links.
#[must_use]
pub fn urls() -> Urls {
    Urls {
        css: url("app.css"),
        js: url("app.js"),
        htmx: url("htmx.min.js"),
        pele: url("pele.global.js"),
    }
}

/// Whether `path` is under the assets prefix.
#[must_use]
pub fn is_asset_path(path: &str) -> bool {
    path.starts_with("/admin/assets/")
}

/// The reply for an asset `path`: the file with a year-long immutable cache,
/// or `404` for an unknown file or a stale hash.
#[must_use]
pub fn serve(path: &str) -> Response {
    let Some(i) = PATHS.iter().position(|p| p == path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let asset = &FILES[i];
    let mut response = asset.bytes.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(asset.content_type),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_file_has_a_hashed_url() {
        let urls = urls();
        for (url, name) in [
            (urls.css, "app.css"),
            (urls.js, "app.js"),
            (urls.htmx, "htmx.min.js"),
        ] {
            assert!(url.starts_with("/admin/assets/"), "{url}");
            assert!(url.ends_with(&format!("/{name}")), "{url}");
            assert_eq!(url.len(), "/admin/assets/".len() + 16 + 1 + name.len());
            assert_eq!(serve(url).status(), StatusCode::OK);
        }
        assert_eq!(url("nope"), "");
        assert_eq!(
            serve("/admin/assets/x/app.css").status(),
            StatusCode::NOT_FOUND
        );
    }
}
