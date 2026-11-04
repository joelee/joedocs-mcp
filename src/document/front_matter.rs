//! YAML front matter: the reserved-key rules, parsing and serialising.
//!
//! Keys fall into three classes (see `docs/plan.md` section 5):
//!
//! - **server-owned** ([`SERVER_OWNED`]): written by the server only;
//! - **validated** (`status`, `tags`, `aliases`): settable but checked, and
//!   never removable;
//! - **custom**: everything else, settable and removable.

use indexmap::IndexMap;
use serde_json::Value;

/// A front matter mapping, in key order. Values are scalars or lists of
/// scalars; nested objects are rejected by [`apply`].
pub type Fields = IndexMap<String, Value>;

/// Keys only the server writes, in the order they are serialised.
pub const SERVER_OWNED: [&str; 10] = [
    "id",
    "title",
    "project",
    "type",
    "revision",
    "created",
    "created_by",
    "updated",
    "updated_by",
    "client",
];

/// Keys clients may set but never remove.
pub const VALIDATED: [&str; 3] = ["status", "tags", "aliases"];

/// Longest custom key, in bytes.
pub const MAX_KEY_LEN: usize = 64;

/// A front matter rule was broken, or the YAML could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct FrontMatterError(pub String);

fn fail<T>(message: impl Into<String>) -> Result<T, FrontMatterError> {
    Err(FrontMatterError(message.into()))
}

/// The server-owned keys and `status`, written first in every file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// Document ID, e.g. `JD-42`.
    pub id: String,
    /// Title as given by the client.
    pub title: String,
    /// Project name.
    pub project: String,
    /// Document type.
    pub doc_type: String,
    /// Status, one of `documents.statuses`.
    pub status: String,
    /// Revision number, starting at 1.
    pub revision: u64,
    /// Creation time (RFC 3339).
    pub created: String,
    /// API key name that created the document.
    pub created_by: String,
    /// Time of the latest revision (RFC 3339).
    pub updated: String,
    /// API key name that wrote the latest revision.
    pub updated_by: String,
    /// MCP client name that wrote the latest revision.
    pub client: String,
}

impl Header {
    fn fields(&self) -> Fields {
        let mut map = Fields::new();
        let mut put = |key: &str, value: Value| {
            map.insert(key.to_owned(), value);
        };
        put("id", self.id.clone().into());
        put("title", self.title.clone().into());
        put("project", self.project.clone().into());
        put("type", self.doc_type.clone().into());
        put("status", self.status.clone().into());
        put("revision", self.revision.into());
        put("created", self.created.clone().into());
        put("created_by", self.created_by.clone().into());
        put("updated", self.updated.clone().into());
        put("updated_by", self.updated_by.clone().into());
        put("client", self.client.clone().into());
        map
    }
}

/// Serialises a whole document: `---`, the header keys, `aliases` and
/// `tags` if present, the other `extra` keys in their order, `---`, then
/// `body` unchanged. Reserved names in `extra` are ignored.
///
/// # Errors
///
/// Returns an error if the YAML serialiser fails.
pub fn render(header: &Header, extra: &Fields, body: &str) -> Result<String, FrontMatterError> {
    let mut map = header.fields();
    for key in ["aliases", "tags"] {
        if let Some(value) = extra.get(key) {
            map.insert(key.to_owned(), value.clone());
        }
    }
    for (key, value) in extra {
        if !is_reserved(key) {
            map.insert(key.clone(), value.clone());
        }
    }
    let yaml = serde_saphyr::to_string(&map)
        .map_err(|e| FrontMatterError(format!("cannot write front matter: {e}")))?;
    let newline = if yaml.ends_with('\n') { "" } else { "\n" };
    Ok(format!("---\n{yaml}{newline}---\n{body}"))
}

/// A document split into its front matter and body.
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed<'a> {
    /// Every front matter key, in file order.
    pub fields: Fields,
    /// Everything after the closing `---` line.
    pub body: &'a str,
}

/// Splits `text` into front matter and body.
///
/// # Errors
///
/// Returns an error when the text does not start with a `---` block or the
/// YAML is not a mapping.
pub fn parse(text: &str) -> Result<Parsed<'_>, FrontMatterError> {
    let Some(rest) = text.strip_prefix("---\n") else {
        return fail("the document does not start with a front matter block");
    };
    let (yaml, body) = if let Some(body) = rest.strip_prefix("---\n") {
        ("", body)
    } else if let Some(end) = rest.find("\n---\n") {
        (&rest[..=end], &rest[end + 5..])
    } else if let Some(yaml) = rest.strip_suffix("\n---") {
        (yaml, "")
    } else {
        return fail("the front matter block is not closed with ---");
    };
    let fields = if yaml.trim().is_empty() {
        Fields::new()
    } else {
        serde_saphyr::from_str::<Fields>(yaml).map_err(|e| {
            let first = e.to_string();
            let first = first.lines().next().unwrap_or_default().to_owned();
            FrontMatterError(format!("invalid front matter YAML: {first}"))
        })?
    };
    Ok(Parsed { fields, body })
}

/// The non-header keys of `fields`: validated `tags` and `aliases` plus
/// every custom key.
#[must_use]
pub fn extras(fields: &Fields) -> Fields {
    fields
        .iter()
        .filter(|(key, _)| !SERVER_OWNED.contains(&key.as_str()) && *key != "status")
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// Whether `key` is server-owned or validated.
#[must_use]
pub fn is_reserved(key: &str) -> bool {
    SERVER_OWNED.contains(&key) || VALIDATED.contains(&key)
}

/// Applies a client's change to `status` and the `extra` keys. Every rule
/// is checked before anything changes, so on error nothing has changed.
///
/// # Errors
///
/// Returns the first broken rule: a server-owned key, removing a validated
/// or unset key, a key both set and removed, a bad key name, `null`, a
/// nested value, an unknown status, or malformed `tags` or `aliases`.
pub fn apply(
    status: &mut String,
    extra: &mut Fields,
    set: &Fields,
    remove: &[String],
    statuses: &[String],
) -> Result<(), FrontMatterError> {
    for key in remove {
        check_not_owned(key)?;
        if VALIDATED.contains(&key.as_str()) {
            return fail(format!("\"{key}\" cannot be removed"));
        }
        if set.contains_key(key) {
            return fail(format!("\"{key}\" is both set and removed"));
        }
        if !extra.contains_key(key) {
            return fail(format!("\"{key}\" is not set, so it cannot be removed"));
        }
    }
    for (key, value) in set {
        check_not_owned(key)?;
        match key.as_str() {
            "status" => check_status(value, statuses)?,
            "tags" => check_list(key, value, check_tag)?,
            "aliases" => check_list(key, value, check_alias)?,
            _ => {
                check_key_name(key)?;
                check_custom_value(key, value)?;
            }
        }
    }

    for key in remove {
        extra.shift_remove(key);
    }
    for (key, value) in set {
        if key == "status" {
            if let Some(value) = value.as_str() {
                value.clone_into(status);
            }
        } else {
            extra.insert(key.clone(), value.clone());
        }
    }
    Ok(())
}

/// Adds `alias` to the `aliases` list unless it is already there.
pub fn add_alias(extra: &mut Fields, alias: &str) {
    let entry = extra
        .entry("aliases".to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Value::Array(list) = entry
        && !list.iter().any(|v| v.as_str() == Some(alias))
    {
        list.push(alias.into());
    }
}

fn check_not_owned(key: &str) -> Result<(), FrontMatterError> {
    match key {
        "title" => fail("\"title\" is owned by the server; use rename_document to change it"),
        "type" => fail("\"type\" is owned by the server; use move_document to change it"),
        _ if SERVER_OWNED.contains(&key) => fail(format!("\"{key}\" is owned by the server")),
        _ => Ok(()),
    }
}

fn check_status(value: &Value, statuses: &[String]) -> Result<(), FrontMatterError> {
    match value.as_str() {
        Some(status) if statuses.iter().any(|s| s == status) => Ok(()),
        _ => fail(format!(
            "status {value} is not one of: {}",
            statuses.join(", ")
        )),
    }
}

fn check_list(
    key: &str,
    value: &Value,
    item: fn(&str) -> Result<(), String>,
) -> Result<(), FrontMatterError> {
    let Value::Array(list) = value else {
        return fail(format!("\"{key}\" must be a list of strings"));
    };
    for entry in list {
        let Some(text) = entry.as_str() else {
            return fail(format!("\"{key}\" must be a list of strings"));
        };
        item(text).map_err(|message| FrontMatterError(format!("{key}: \"{text}\" {message}")))?;
    }
    Ok(())
}

fn check_tag(tag: &str) -> Result<(), String> {
    let valid = !tag.is_empty()
        && tag
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '/'))
        && !tag.chars().all(|c| c.is_ascii_digit());
    if valid {
        Ok(())
    } else {
        Err("must be letters, digits, '_', '-' or '/', and not only digits".to_owned())
    }
}

fn check_alias(alias: &str) -> Result<(), String> {
    super::check_title(alias).map(|_| ())
}

fn check_key_name(key: &str) -> Result<(), FrontMatterError> {
    let bytes = key.as_bytes();
    let valid = bytes
        .first()
        .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
        && bytes.len() <= MAX_KEY_LEN
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        fail(format!(
            "key \"{key}\" must be up to {MAX_KEY_LEN} ASCII letters, digits, '_' or '-', not starting with '-'"
        ))
    }
}

fn check_custom_value(key: &str, value: &Value) -> Result<(), FrontMatterError> {
    let scalar = |v: &Value| matches!(v, Value::String(_) | Value::Number(_) | Value::Bool(_));
    match value {
        Value::Null => fail(format!("\"{key}\" is null; use remove to delete a key")),
        Value::Array(list) if !list.iter().all(scalar) => fail(format!(
            "\"{key}\" must be a list of strings, numbers or booleans"
        )),
        Value::Object(_) => fail(format!(
            "\"{key}\" is a nested object, which is not supported"
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn header() -> Header {
        Header {
            id: "JD-42".into(),
            title: "Search: query".into(),
            project: "joedocs".into(),
            doc_type: "specs".into(),
            status: "draft".into(),
            revision: 4,
            created: "2026-11-04T09:12:03Z".into(),
            created_by: "laptop".into(),
            updated: "2026-11-05T14:40:10Z".into(),
            updated_by: "desktop".into(),
            client: "claude-code".into(),
        }
    }

    fn fields(value: Value) -> Fields {
        match value {
            Value::Object(map) => map.into_iter().collect(),
            _ => unreachable!("test fields must be an object"),
        }
    }

    const STATUSES: &[&str] = &["draft", "approved"];

    fn statuses() -> Vec<String> {
        STATUSES.iter().map(|s| (*s).to_owned()).collect()
    }

    fn try_apply(set: Value, remove: &[&str]) -> Result<(String, Fields), FrontMatterError> {
        let mut status = "draft".to_owned();
        let mut extra = fields(json!({"tags": ["a"], "owner": "joel"}));
        let remove: Vec<String> = remove.iter().map(|s| (*s).to_owned()).collect();
        apply(&mut status, &mut extra, &fields(set), &remove, &statuses())?;
        Ok((status, extra))
    }

    fn apply_err(set: Value, remove: &[&str]) -> String {
        try_apply(set, remove).unwrap_err().0
    }

    #[test]
    fn render_orders_header_then_obsidian_then_custom_keys() {
        let extra =
            fields(json!({"owner": "joel", "tags": ["x"], "id": "spoofed", "aliases": ["Old"]}));
        let text = render(&header(), &extra, "# Body\n").unwrap();
        assert_eq!(
            text,
            "---\nid: JD-42\ntitle: \"Search: query\"\nproject: joedocs\ntype: specs\n\
             status: draft\nrevision: 4\ncreated: \"2026-11-04T09:12:03Z\"\ncreated_by: laptop\n\
             updated: \"2026-11-05T14:40:10Z\"\nupdated_by: desktop\nclient: claude-code\n\
             aliases:\n- Old\ntags:\n- x\nowner: joel\n---\n# Body\n"
        );
    }

    #[test]
    fn parse_reverses_render() {
        let extra = fields(json!({"tags": ["x"], "n": 1.5, "multi": "a\n---\nb"}));
        let text = render(&header(), &extra, "body\n---\nmore\n").unwrap();
        let parsed = parse(&text).unwrap();
        assert_eq!(parsed.body, "body\n---\nmore\n");
        assert_eq!(parsed.fields["revision"], json!(4));
        assert_eq!(extras(&parsed.fields), extra);
    }

    #[test]
    fn parse_handles_empty_blocks_and_missing_bodies() {
        let parsed = parse("---\n---\nbody").unwrap();
        assert!(parsed.fields.is_empty());
        assert_eq!(parsed.body, "body");
        let parsed = parse("---\na: 1\n---").unwrap();
        assert_eq!(parsed.fields["a"], json!(1));
        assert_eq!(parsed.body, "");
    }

    #[test]
    fn parse_rejects_files_without_valid_front_matter() {
        assert!(parse("# Title\n").unwrap_err().0.contains("does not start"));
        assert!(parse("---\na: 1\n").unwrap_err().0.contains("not closed"));
        let err = parse("---\na: [1\n---\n").unwrap_err().0;
        assert!(err.starts_with("invalid front matter YAML: "), "{err}");
        assert!(!err.contains('\n'), "{err}");
        assert!(parse("---\n- a\n---\n").is_err());
    }

    #[test]
    fn apply_sets_and_removes_custom_keys_in_place() {
        let (status, extra) = try_apply(
            json!({"owner": "sam", "due": "2026-12-01", "status": "approved"}),
            &[],
        )
        .unwrap();
        assert_eq!(status, "approved");
        // json! sorts its keys, so the starting order is owner, tags.
        assert_eq!(
            extra.keys().collect::<Vec<_>>(),
            ["owner", "tags", "due"],
            "existing keys keep their place; new keys go last"
        );
        assert_eq!(extra["owner"], json!("sam"));

        let (_, extra) = try_apply(json!({}), &["owner"]).unwrap();
        assert_eq!(extra, fields(json!({"tags": ["a"]})));
    }

    #[test]
    fn apply_protects_reserved_keys() {
        assert!(apply_err(json!({"title": "x"}), &[]).contains("rename_document"));
        assert!(apply_err(json!({"type": "x"}), &[]).contains("move_document"));
        assert_eq!(
            apply_err(json!({"revision": 9}), &[]),
            "\"revision\" is owned by the server"
        );
        assert_eq!(
            apply_err(json!({}), &["id"]),
            "\"id\" is owned by the server"
        );
        assert_eq!(
            apply_err(json!({}), &["tags"]),
            "\"tags\" cannot be removed"
        );
        assert_eq!(
            apply_err(json!({}), &["status"]),
            "\"status\" cannot be removed"
        );
    }

    #[test]
    fn apply_rejects_bad_removals() {
        assert_eq!(
            apply_err(json!({"owner": "x"}), &["owner"]),
            "\"owner\" is both set and removed"
        );
        assert!(apply_err(json!({}), &["nope"]).contains("not set"));
    }

    #[test]
    fn apply_validates_values() {
        assert!(apply_err(json!({"status": "open"}), &[]).contains("draft, approved"));
        assert!(apply_err(json!({"status": 1}), &[]).contains("not one of"));
        assert!(apply_err(json!({"tags": "a"}), &[]).contains("list of strings"));
        assert!(apply_err(json!({"tags": [1]}), &[]).contains("list of strings"));
        for bad in ["my tag", "", "#a", "2026"] {
            let err = apply_err(json!({"tags": [bad]}), &[]);
            assert!(err.starts_with(&format!("tags: \"{bad}\"")), "{err}");
        }
        assert!(try_apply(json!({"tags": ["a/b-c_d", "v2"]}), &[]).is_ok());
        assert!(apply_err(json!({"aliases": ["a\nb"]}), &[]).starts_with("aliases:"));
        assert!(apply_err(json!({"owner": null}), &[]).contains("use remove"));
        assert!(apply_err(json!({"owner": {"a": 1}}), &[]).contains("nested object"));
        assert!(apply_err(json!({"owner": [[1]]}), &[]).contains("list of strings"));
        assert!(try_apply(json!({"n": 1, "b": true, "l": ["a", 2, false]}), &[]).is_ok());
    }

    #[test]
    fn apply_checks_custom_key_names() {
        for bad in ["", "-a", "a b", "a.b", "é", &"k".repeat(MAX_KEY_LEN + 1)] {
            let err = apply_err(json!({ bad: "x" }), &[]);
            assert!(err.starts_with(&format!("key \"{bad}\"")), "{err}");
        }
        assert!(try_apply(json!({"_x": 1, "approved_by": "joel", "a-b": 2}), &[]).is_ok());
    }

    #[test]
    fn nothing_changes_when_any_rule_fails() {
        let mut status = "draft".to_owned();
        let mut extra = fields(json!({"owner": "joel"}));
        let set = fields(json!({"status": "approved", "bad key": 1}));
        assert!(apply(&mut status, &mut extra, &set, &[], &statuses()).is_err());
        assert_eq!(status, "draft");
        assert_eq!(extra, fields(json!({"owner": "joel"})));
    }

    #[test]
    fn aliases_are_added_once() {
        let mut extra = Fields::new();
        add_alias(&mut extra, "Old");
        add_alias(&mut extra, "Old");
        add_alias(&mut extra, "Older");
        assert_eq!(extra, fields(json!({"aliases": ["Old", "Older"]})));
    }

    #[test]
    fn reserved_keys_are_recognised() {
        assert!(is_reserved("id"));
        assert!(is_reserved("tags"));
        assert!(!is_reserved("owner"));
    }
}
