//! Front matter filters and their SQL translation.
//!
//! Filters are a JSON object keyed by front matter key. A plain value means
//! equality; an object holds operators that must all hold: `eq`, `ne`,
//! `in`, `contains`, `gt`, `gte`, `lt`, `lte` and `exists`.
//!
//! Every front matter key is flattened into `doc_fields` with one row per
//! value (a list has a row per item), so each condition asks whether *some*
//! value of the key satisfies it. `contains` is therefore the same as `eq`
//! and reads better for lists such as `tags`; `ne` means no value equals.
//! Text compares without ASCII case. Numbers compare as numbers, other
//! values as text, which orders RFC 3339 dates such as `2026-10-01`
//! correctly.

use rusqlite::types::Value as SqlValue;
use serde_json::{Map, Value};

use super::Invalid;
use crate::document::front_matter::is_key_name;

/// Most conditions in one search.
pub const MAX_CONDITIONS: usize = 32;

/// Most values in one `in` list.
pub const MAX_IN_VALUES: usize = 100;

const OPERATORS: &str = "eq, ne, in, contains, gt, gte, lt, lte, exists";

/// A value to compare with.
#[derive(Debug, Clone, PartialEq)]
enum Scalar {
    Text(String),
    Number(f64),
}

impl Scalar {
    fn from_json(value: &Value) -> Option<Self> {
        match value {
            Value::String(s) => Some(Self::Text(s.clone())),
            Value::Bool(b) => Some(Self::Text(b.to_string())),
            Value::Number(n) => n.as_f64().map(Self::Number),
            _ => None,
        }
    }

    /// The column to compare and the bound value.
    fn column(&self) -> (&'static str, SqlValue) {
        match self {
            Self::Text(s) => ("f.value", SqlValue::Text(s.clone())),
            Self::Number(n) => ("f.num", SqlValue::Real(*n)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compare {
    Eq,
    Gt,
    Gte,
    Lt,
    Lte,
}

impl Compare {
    fn sql(self) -> &'static str {
        match self {
            Self::Eq => "=",
            Self::Gt => ">",
            Self::Gte => ">=",
            Self::Lt => "<",
            Self::Lte => "<=",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Test {
    /// Some value compares true.
    Compare(Compare, Scalar),
    /// No value equals.
    NotEqual(Scalar),
    /// Some value equals one of these.
    In(Vec<Scalar>),
    /// The key is present (or absent).
    Exists(bool),
}

/// One condition on one front matter key.
#[derive(Debug, Clone, PartialEq)]
pub struct Condition {
    key: String,
    test: Test,
}

/// Parses the `filters` object.
///
/// # Errors
///
/// Returns [`Invalid`] for a bad key, an unknown operator, a value of the
/// wrong kind, or too many conditions.
pub fn parse(filters: &Map<String, Value>) -> Result<Vec<Condition>, Invalid> {
    let mut conditions = Vec::new();
    for (key, value) in filters {
        if !is_key_name(key) {
            return Err(Invalid(format!("\"{key}\" is not a front matter key")));
        }
        let mut push = |test| {
            conditions.push(Condition {
                key: key.clone(),
                test,
            });
        };
        match value {
            Value::Object(ops) if ops.is_empty() => {
                return Err(Invalid(format!(
                    "{key}: give a value or operators ({OPERATORS})"
                )));
            }
            Value::Object(ops) => {
                for (op, operand) in ops {
                    push(operator(key, op, operand)?);
                }
            }
            Value::Array(_) => {
                return Err(Invalid(format!(
                    "{key}: a list is ambiguous; use {{\"in\": [...]}} for any of the values \
                     or one {{\"contains\": ...}} per value for all of them"
                )));
            }
            Value::Null => {
                return Err(Invalid(format!(
                    "{key}: null matches nothing; use {{\"exists\": false}}"
                )));
            }
            scalar => push(Test::Compare(Compare::Eq, self::scalar(key, "", scalar)?)),
        }
    }
    if conditions.len() > MAX_CONDITIONS {
        return Err(Invalid(format!(
            "must have at most {MAX_CONDITIONS} conditions"
        )));
    }
    Ok(conditions)
}

fn scalar(key: &str, op: &str, value: &Value) -> Result<Scalar, Invalid> {
    Scalar::from_json(value).ok_or_else(|| {
        let at = if op.is_empty() {
            key.to_owned()
        } else {
            format!("{key}.{op}")
        };
        Invalid(format!("{at}: must be a string, number or boolean"))
    })
}

fn operator(key: &str, op: &str, operand: &Value) -> Result<Test, Invalid> {
    let compare = |c| Ok(Test::Compare(c, scalar(key, op, operand)?));
    match op {
        "eq" | "contains" => compare(Compare::Eq),
        "gt" => compare(Compare::Gt),
        "gte" => compare(Compare::Gte),
        "lt" => compare(Compare::Lt),
        "lte" => compare(Compare::Lte),
        "ne" => Ok(Test::NotEqual(scalar(key, op, operand)?)),
        "in" => {
            let Value::Array(items) = operand else {
                return Err(Invalid(format!("{key}.in: must be a list")));
            };
            if items.is_empty() || items.len() > MAX_IN_VALUES {
                return Err(Invalid(format!(
                    "{key}.in: must have 1 to {MAX_IN_VALUES} values"
                )));
            }
            let values = items
                .iter()
                .map(|item| scalar(key, op, item))
                .collect::<Result<_, _>>()?;
            Ok(Test::In(values))
        }
        "exists" => match operand {
            Value::Bool(present) => Ok(Test::Exists(*present)),
            _ => Err(Invalid(format!("{key}.exists: must be true or false"))),
        },
        other => Err(Invalid(format!(
            "{key}: unknown operator \"{other}\"; use one of {OPERATORS}"
        ))),
    }
}

impl Condition {
    /// A SQL condition on documents aliased `d`, pushing its parameters.
    pub fn to_sql(&self, params: &mut Vec<SqlValue>) -> String {
        params.push(SqlValue::Text(self.key.clone()));
        let rows = "SELECT 1 FROM doc_fields f WHERE f.document_id = d.id AND f.file_id IS NULL AND f.key = ?";
        match &self.test {
            Test::Exists(true) => format!("EXISTS ({rows})"),
            Test::Exists(false) => format!("NOT EXISTS ({rows})"),
            Test::Compare(compare, value) => {
                let (column, bound) = value.column();
                params.push(bound);
                format!("EXISTS ({rows} AND {column} {} ?)", compare.sql())
            }
            Test::NotEqual(value) => {
                let (column, bound) = value.column();
                params.push(bound);
                format!("NOT EXISTS ({rows} AND {column} = ?)")
            }
            Test::In(values) => {
                let tests: Vec<String> = values
                    .iter()
                    .map(|value| {
                        let (column, bound) = value.column();
                        params.push(bound);
                        format!("{column} = ?")
                    })
                    .collect();
                format!("EXISTS ({rows} AND ({}))", tests.join(" OR "))
            }
        }
    }
}

/// The `doc_fields` rows for one front matter value: its text and, for
/// numbers, its numeric value. Lists give a row per item; objects and nulls
/// give none.
#[must_use]
pub fn flatten(value: &Value) -> Vec<(String, Option<f64>)> {
    match value {
        Value::String(s) => vec![(s.clone(), None)],
        Value::Bool(b) => vec![(b.to_string(), None)],
        Value::Number(n) => vec![(n.to_string(), n.as_f64())],
        Value::Array(items) => items
            .iter()
            .filter(|item| !item.is_array())
            .flat_map(flatten)
            .collect(),
        Value::Null | Value::Object(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sql(filters: &Value) -> Result<(String, Vec<SqlValue>), Invalid> {
        let conditions = parse(filters.as_object().unwrap())?;
        let mut params = Vec::new();
        let parts: Vec<String> = conditions.iter().map(|c| c.to_sql(&mut params)).collect();
        Ok((parts.join(" AND "), params))
    }

    fn err(filters: &Value) -> String {
        sql(filters).unwrap_err().0
    }

    const ROWS: &str =
        "SELECT 1 FROM doc_fields f WHERE f.document_id = d.id AND f.file_id IS NULL AND f.key = ?";

    #[test]
    fn plain_values_mean_equality() {
        let (text, params) =
            sql(&json!({"status": "approved", "revision": 3, "draft": true})).unwrap();
        // serde_json maps iterate in key order.
        assert_eq!(
            text,
            format!(
                "EXISTS ({ROWS} AND f.value = ?) AND EXISTS ({ROWS} AND f.num = ?) \
                 AND EXISTS ({ROWS} AND f.value = ?)"
            )
        );
        assert_eq!(
            params,
            [
                SqlValue::Text("draft".into()),
                SqlValue::Text("true".into()),
                SqlValue::Text("revision".into()),
                SqlValue::Real(3.0),
                SqlValue::Text("status".into()),
                SqlValue::Text("approved".into()),
            ]
        );
    }

    #[test]
    fn operators_translate() {
        let (text, params) = sql(&json!({
            "updated": {"gte": "2026-10-01", "lt": "2026-11-01"},
            "tags": {"contains": "security", "ne": "wip"},
            "owner": {"in": ["joel", "ai"]},
            "approved_by": {"exists": false},
            "due": {"exists": true, "gt": 1, "lte": 9},
        }))
        .unwrap();
        assert_eq!(
            text,
            format!(
                "NOT EXISTS ({ROWS}) \
                 AND EXISTS ({ROWS}) AND EXISTS ({ROWS} AND f.num > ?) \
                 AND EXISTS ({ROWS} AND f.num <= ?) \
                 AND EXISTS ({ROWS} AND (f.value = ? OR f.value = ?)) \
                 AND EXISTS ({ROWS} AND f.value = ?) AND NOT EXISTS ({ROWS} AND f.value = ?) \
                 AND EXISTS ({ROWS} AND f.value >= ?) AND EXISTS ({ROWS} AND f.value < ?)"
            )
        );
        assert_eq!(params.len(), 17);
    }

    #[test]
    fn bad_filters_are_explained() {
        assert!(err(&json!({"bad key": 1})).contains("not a front matter key"));
        assert!(err(&json!({"tags": []})).contains("ambiguous"));
        assert!(err(&json!({"tags": null})).contains("exists"));
        assert!(err(&json!({"tags": {}})).contains("give a value"));
        assert!(err(&json!({"tags": {"like": "x"}})).contains("unknown operator \"like\""));
        assert!(err(&json!({"tags": {"eq": {"a": 1}}})).contains("tags.eq: must be"));
        assert!(err(&json!({"tags": {"eq": ["a"]}})).contains("tags.eq: must be"));
        assert!(err(&json!({"tags": {"in": "a"}})).contains("must be a list"));
        assert!(err(&json!({"tags": {"in": []}})).contains("1 to 100"));
        assert!(err(&json!({"tags": {"in": [null]}})).contains("tags.in: must be"));
        assert!(err(&json!({"tags": {"exists": "yes"}})).contains("true or false"));
        let many: Map<String, Value> = (0..=MAX_CONDITIONS)
            .map(|i| (format!("k{i}"), json!(1)))
            .collect();
        assert!(err(&Value::Object(many)).contains("at most 32"));
    }

    #[test]
    fn values_flatten_to_rows() {
        assert_eq!(flatten(&json!("a")), [("a".to_owned(), None)]);
        assert_eq!(flatten(&json!(false)), [("false".to_owned(), None)]);
        assert_eq!(flatten(&json!(2)), [("2".to_owned(), Some(2.0))]);
        assert_eq!(flatten(&json!(1.5)), [("1.5".to_owned(), Some(1.5))]);
        assert_eq!(
            flatten(&json!(["x", 1, ["nested"], null])),
            [("x".to_owned(), None), ("1".to_owned(), Some(1.0))]
        );
        assert_eq!(flatten(&json!(null)), Vec::new());
        assert_eq!(flatten(&json!({"a": 1})), Vec::new());
    }
}
