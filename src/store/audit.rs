//! Reading the audit log: one row per MCP write (or attempted write) and per
//! admin API call.

use super::documents::parse_id;
use super::{Error, Store};
use crate::db::{self, AuditQuery, AuditRow};

/// Most audit entries one page may return.
pub const MAX_AUDIT_LIMIT: u64 = 1000;

/// What to read from the audit log; `None` means any.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditRequest {
    /// Only entries at or after this UTC date or time.
    pub since: Option<String>,
    /// Only entries before this UTC date or time.
    pub until: Option<String>,
    /// Only this key (or admin token) name.
    pub key: Option<String>,
    /// Only this tool; ending in `*`, tools that start with the rest.
    pub tool: Option<String>,
    /// Only calls about this document.
    pub document: Option<String>,
    /// Only this outcome: `ok` or an error code.
    pub outcome: Option<String>,
    /// 1-based position of the first entry, newest first.
    pub start: u64,
    /// Most entries to return, 1 to [`MAX_AUDIT_LIMIT`].
    pub limit: u64,
}

/// One page of audit entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditPage {
    /// Newest first.
    pub items: Vec<AuditRow>,
    /// How many entries match.
    pub total: u64,
    /// `start` for the next page, if there is one.
    pub next_start: Option<u64>,
}

impl Store {
    /// Reads a page of the audit log, newest first.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] for a time that is not a UTC date or
    /// timestamp, a bad document ID, start or limit, and database errors.
    pub fn audit_entries(&self, request: &AuditRequest) -> Result<AuditPage, Error> {
        if request.start == 0 {
            return Err(Error::invalid("start", "must be 1 or more"));
        }
        if request.limit == 0 || request.limit > MAX_AUDIT_LIMIT {
            return Err(Error::invalid(
                "limit",
                format!("must be 1 to {MAX_AUDIT_LIMIT}"),
            ));
        }
        let since = request
            .since
            .as_deref()
            .map(|t| audit_time("since", t))
            .transpose()?;
        let until = request
            .until
            .as_deref()
            .map(|t| audit_time("until", t))
            .transpose()?;
        let document = request
            .document
            .as_deref()
            .map(|id| parse_id(id).map(|id| id.to_string()))
            .transpose()?;
        let (tool, tool_prefix) = match request.tool.as_deref() {
            Some(tool) => match tool.strip_suffix('*') {
                Some(prefix) => (None, Some(prefix)),
                None => (Some(tool), None),
            },
            None => (None, None),
        };
        let offset = request.start - 1;
        let (items, total) = db::list_audit(
            &self.conn,
            &AuditQuery {
                since: since.as_deref(),
                until: until.as_deref(),
                key_name: request.key.as_deref(),
                tool,
                tool_prefix,
                document_id: document.as_deref(),
                outcome: request.outcome.as_deref(),
                offset,
                limit: request.limit,
            },
        )?;
        let shown = offset + items.len() as u64;
        Ok(AuditPage {
            items,
            total,
            next_start: (shown < total).then_some(shown + 1),
        })
    }
}

/// `text` as a UTC timestamp in the audit log's format: a date
/// (`2026-11-10`, meaning its midnight) or a timestamp
/// (`2026-11-10T14:00:00Z`).
fn audit_time(field: &'static str, text: &str) -> Result<String, Error> {
    let full = match text.len() {
        10 => format!("{text}T00:00:00Z"),
        20 => text.to_owned(),
        _ => String::new(),
    };
    let shaped = full.len() == 20
        && full.bytes().enumerate().all(|(i, b)| match i {
            4 | 7 => b == b'-',
            10 => b == b'T',
            13 | 16 => b == b':',
            19 => b == b'Z',
            _ => b.is_ascii_digit(),
        });
    match full.parse::<jiff::Timestamp>() {
        Ok(at) if shaped => Ok(crate::clock::format_timestamp(at)),
        _ => Err(Error::invalid(
            field,
            format!("\"{text}\" is not a UTC date (2026-11-10) or time (2026-11-10T14:00:00Z)"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_times_are_dates_or_utc_timestamps() {
        assert_eq!(
            audit_time("since", "2026-11-10").unwrap(),
            "2026-11-10T00:00:00Z"
        );
        assert_eq!(
            audit_time("since", "2026-11-10T14:05:09Z").unwrap(),
            "2026-11-10T14:05:09Z"
        );
        for bad in [
            "yesterday",
            "2026-13-01",
            "2026-11-10T25:00:00Z",
            "2026-11-10 14:00:00Z",
            "2026-11-10T14:00:00+01",
            "",
        ] {
            let error = audit_time("since", bad).unwrap_err();
            assert!(error.to_string().contains("since"), "{bad}: {error}");
        }
    }
}
