//! Changes other than whole-body updates: moving to another type, exact
//! in-place edits, appending, and restoring an earlier revision.

use super::documents::{
    Action, Changed, integrity, live_row, parse_id, revision_with_content, tidy_line,
};
use super::{Actor, Error, Store};
use crate::document::front_matter;

/// Most replacements in one `edit`.
pub const MAX_EDITS: usize = 64;

/// One exact replacement in a document's body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replacement {
    /// Text that must occur exactly once in the body.
    pub old: String,
    /// What replaces it.
    pub new: String,
}

impl Store {
    /// Changes a document's type, moving its file to that type's folder.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] for an unknown or unchanged type, and the
    /// errors of [`Store::update`].
    pub fn move_document(
        &mut self,
        actor: &Actor,
        id: &str,
        doc_type: &str,
        expected_revision: Option<u64>,
    ) -> Result<Changed, Error> {
        super::documents::check_member("type", doc_type, &self.settings.types)?;
        let doc_type = doc_type.to_owned();
        self.revise(
            actor,
            id,
            expected_revision,
            Action::Move,
            None,
            |draft, _| {
                if draft.doc_type == doc_type {
                    return Err(Error::invalid("type", "is already the document's type"));
                }
                draft.doc_type = doc_type;
                Ok(())
            },
        )
    }

    /// Applies exact replacements to a document's body, in order. Each
    /// `old` must occur exactly once when its turn comes; if any edit
    /// fails, nothing changes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] naming the edit that failed, and the
    /// errors of [`Store::update`].
    pub fn edit(
        &mut self,
        actor: &Actor,
        id: &str,
        edits: &[Replacement],
        expected_revision: Option<u64>,
        summary: Option<&str>,
    ) -> Result<Changed, Error> {
        if edits.is_empty() || edits.len() > MAX_EDITS {
            return Err(Error::invalid(
                "edits",
                format!("give 1 to {MAX_EDITS} edits"),
            ));
        }
        self.revise(
            actor,
            id,
            expected_revision,
            Action::Edit,
            tidy_line(summary),
            |draft, _| {
                draft.body = apply_edits(&draft.body, edits)?;
                Ok(())
            },
        )
    }

    /// Adds `content` to the end of a document's body, after a blank line.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] for empty content, and the errors of
    /// [`Store::update`].
    pub fn append(
        &mut self,
        actor: &Actor,
        id: &str,
        content: &str,
        expected_revision: Option<u64>,
        summary: Option<&str>,
    ) -> Result<Changed, Error> {
        if content.trim().is_empty() {
            return Err(Error::invalid("content", "must not be empty"));
        }
        self.revise(
            actor,
            id,
            expected_revision,
            Action::Append,
            tidy_line(summary),
            |draft, _| {
                draft.body = appended(&draft.body, content);
                Ok(())
            },
        )
    }

    /// Makes an earlier revision's body, status, tags and custom keys the
    /// new latest revision. The title, type and aliases stay as they are,
    /// since only rename and move change those. The history is not
    /// rewritten.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] for a revision that does not exist, has no
    /// content or is already the latest, and the errors of
    /// [`Store::update`].
    pub fn restore_version(
        &mut self,
        actor: &Actor,
        id: &str,
        version: u64,
        expected_revision: Option<u64>,
        summary: Option<&str>,
    ) -> Result<Changed, Error> {
        let row = live_row(&self.conn, &parse_id(id)?)?;
        if version == row.revision {
            return Err(Error::invalid(
                "version",
                format!("{version} is already the latest revision"),
            ));
        }
        let revision = revision_with_content(&self.conn, &row, version, "version")?;
        let text = self.committed_text(&row, &revision)?;
        let parsed = front_matter::parse(&text).map_err(|_| integrity(&row))?;
        let status = parsed
            .fields
            .get("status")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let mut extra = front_matter::extras(&parsed.fields);
        let body = parsed.body.to_owned();
        let summary = tidy_line(summary).unwrap_or_else(|| format!("revision {version}"));
        self.revise(
            actor,
            id,
            expected_revision,
            Action::Restore,
            Some(summary),
            move |draft, settings| {
                match draft.extra.get("aliases") {
                    Some(aliases) => {
                        extra.insert("aliases".to_owned(), aliases.clone());
                    }
                    None => {
                        extra.shift_remove("aliases");
                    }
                }
                draft.extra = extra;
                if let Some(status) = status.filter(|s| settings.statuses.contains(s)) {
                    draft.status = status;
                }
                draft.body = body;
                Ok(())
            },
        )
    }
}

/// Applies `edits` to `body` in order.
fn apply_edits(body: &str, edits: &[Replacement]) -> Result<String, Error> {
    let mut body = body.to_owned();
    for (index, edit) in edits.iter().enumerate() {
        let n = index + 1;
        if edit.old.is_empty() {
            return Err(Error::invalid(
                "edits",
                format!("edit {n}: old must not be empty; use append_to_document to add text"),
            ));
        }
        match body.matches(edit.old.as_str()).count() {
            1 => body = body.replacen(edit.old.as_str(), &edit.new, 1),
            0 => {
                return Err(Error::invalid(
                    "edits",
                    format!(
                        "edit {n}: old was not found in the body; it must match exactly, \
                         including whitespace (earlier edits apply first)"
                    ),
                ));
            }
            count => {
                return Err(Error::invalid(
                    "edits",
                    format!(
                        "edit {n}: old occurs {count} times; include more of the surrounding \
                         text so it occurs once"
                    ),
                ));
            }
        }
    }
    Ok(body)
}

/// `body` with `content` after it, separated by a blank line, ending in a
/// newline.
fn appended(body: &str, content: &str) -> String {
    let mut out = body.trim_end_matches('\n').to_owned();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(content.trim_start_matches('\n'));
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(old: &str, new: &str) -> Replacement {
        Replacement {
            old: old.into(),
            new: new.into(),
        }
    }

    fn message(result: Result<String, Error>) -> String {
        match result {
            Err(Error::Invalid { field, message }) => format!("{field}: {message}"),
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn edits_apply_in_order() {
        let body = "# Plan\n\nStep one.\nStep two.\n";
        assert_eq!(
            apply_edits(body, &[edit("one", "1"), edit("Step 1.\n", "")]).unwrap(),
            "# Plan\n\nStep two.\n"
        );
    }

    #[test]
    fn edits_must_match_exactly_once() {
        let body = "a b a";
        assert!(
            message(apply_edits(body, &[edit("c", "x")])).contains("edit 1: old was not found")
        );
        assert!(
            message(apply_edits(body, &[edit("b", "x"), edit("a", "y")]))
                .contains("edit 2: old occurs 2 times")
        );
        assert!(
            message(apply_edits(body, &[edit("", "x")]))
                .contains("edits: edit 1: old must not be empty")
        );
    }

    #[test]
    fn appending_leaves_one_blank_line() {
        assert_eq!(appended("# A\n", "## B"), "# A\n\n## B\n");
        assert_eq!(appended("# A\n\n\n", "\n## B\n"), "# A\n\n## B\n");
        assert_eq!(appended("# A", "B\n"), "# A\n\nB\n");
        assert_eq!(appended("", "B"), "B\n");
    }
}
