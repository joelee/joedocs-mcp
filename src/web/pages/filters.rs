//! The server side of the filter parts (JD-71), shared by the pages that
//! filter with them (the Board and Documents): the choices of a group, the
//! picker-or-pills rule, what the address names, and the refusal of an
//! address naming values the page does not know. The browser side is
//! `filters.js`; the picker's markup is `templates/filter_parts.html`.

use axum::http::StatusCode;

use super::super::assets;
use super::super::{WebError, WebRequest, render};
use crate::store::ProjectRow;

/// The most values a type or status group shows as pills; a longer list
/// is a picker (JD-71 D-08).
pub(crate) const PILLS_MAX: usize = 12;

/// One group of a filter panel: projects, types or statuses.
pub(crate) struct Group {
    pub(crate) choices: Vec<Choice>,
    /// A picker rather than pills (projects always; types and statuses
    /// beyond [`PILLS_MAX`] values).
    pub(crate) picker: bool,
    /// Whether any value is ticked: groups whose default is "all" tick
    /// only what the address names (none ticked means all).
    pub(crate) named: bool,
}

/// A checkbox in a filter panel.
pub(crate) struct Choice {
    pub(crate) value: String,
    pub(crate) label: String,
    pub(crate) checked: bool,
    /// Muted text after the label in a picker: a project's name and key.
    pub(crate) meta: String,
    /// What the picker's search matches, lower case.
    pub(crate) search: String,
}

/// A group of a filter panel: a checkbox per value of `all`, ticked when
/// in `picked`. Projects show their titles, with the name and key in a
/// picker; values of a group longer than [`PILLS_MAX`] are a picker too.
pub(crate) fn group(
    projects: &[ProjectRow],
    all: &[String],
    picked: &[String],
    always_picker: bool,
) -> Group {
    let choices = all
        .iter()
        .map(|v| {
            let project = projects.iter().find(|p| &p.name == v);
            let label = project
                .map(|p| p.title.as_str())
                .filter(|t| !t.is_empty())
                .unwrap_or(v)
                .to_owned();
            let key = project.map_or("", |p| p.key.as_str());
            let meta = match (label == *v, key.is_empty()) {
                (_, true) => String::new(),
                (true, false) => key.to_owned(),
                (false, false) => format!("{v} · {key}"),
            };
            Choice {
                search: format!("{label} {v} {key}").to_lowercase(),
                value: v.clone(),
                label,
                checked: picked.contains(v),
                meta,
            }
        })
        .collect();
    Group {
        choices,
        picker: always_picker || all.len() > PILLS_MAX,
        named: !picked.is_empty(),
    }
}

/// The values of `all` that the address's `key` selects, in `all`'s order;
/// when it names none, the values `default` accepts.
pub(crate) fn chosen(
    request: &WebRequest,
    key: &str,
    all: &[String],
    default: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let asked = request.query_list(key);
    all.iter()
        .filter(|v| {
            if asked.is_empty() {
                default(v)
            } else {
                asked.contains(v)
            }
        })
        .cloned()
        .collect()
}

/// What a group whose default is "all" ticks: only the values the address
/// names, and none when it names none or every one (both mean all).
pub(crate) fn named(request: &WebRequest, key: &str, all: &[String]) -> Vec<String> {
    let asked = request.query_list(key);
    let picked: Vec<String> = all.iter().filter(|v| asked.contains(v)).cloned().collect();
    if picked.len() == all.len() {
        Vec::new()
    } else {
        picked
    }
}

/// The values of the address a page does not know, as `key: value`: in
/// `lists`, any value of a list key (`project`, `type`, `status`) that is
/// not one of its known values; in `singles`, a one-value key (`sort`,
/// `dir`) outside its values. Empty values are not refusals.
pub(crate) fn unknown_values(
    request: &WebRequest,
    lists: &[(&str, &[String])],
    singles: &[(&str, &[&str])],
) -> Vec<String> {
    let mut unknown = Vec::new();
    for (key, known) in lists {
        for value in request.query_list(key) {
            if !known.contains(&value) {
                unknown.push(format!("{key}: {value}"));
            }
        }
    }
    for (key, known) in singles {
        if let Some(value) = request.query(key).filter(|v| !v.is_empty())
            && !known.contains(&value)
        {
            unknown.push(format!("{key}: {value}"));
        }
    }
    unknown
}

/// Which page refuses an address: its name in the message, its path and
/// the remembered view the refusal's link forgets.
pub(crate) struct Refusing<'a> {
    /// "The Board's address", "The Documents page's address".
    pub(crate) address: &'a str,
    pub(crate) path: &'a str,
    /// The link's text, e.g. "Reset the Board's view".
    pub(crate) reset: &'a str,
    /// The remembered view's key in `app.js` (`board`, `documents`).
    pub(crate) view: &'a str,
}

/// The refusal for an address naming `unknown` values: `400
/// invalid_argument`, with a link that forgets the page's remembered view.
pub(crate) fn unknown_refusal(page: &Refusing<'_>, unknown: &[String]) -> WebError {
    let message = format!(
        "{} names something it does not know ({}).",
        page.address,
        unknown.join("; ")
    );
    let shown = super::ErrorPage {
        title: "Refused",
        assets: assets::urls(),
        status: StatusCode::BAD_REQUEST.as_u16(),
        message: &message,
        link: Some((page.path, page.reset)),
        reset_view: Some(page.view),
    };
    let shown = render(StatusCode::BAD_REQUEST, &shown);
    WebError::new(StatusCode::BAD_REQUEST, "invalid_argument", message).with_page(shown)
}
