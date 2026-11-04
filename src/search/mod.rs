//! Search: the forgiving query grammar, generation of FTS5 match
//! expressions, and translation of front matter filters to SQL. Pure: the
//! queries run in [`crate::db`]. See `docs/plan.md` section 7.

pub mod filter;
pub mod query;

pub use filter::{Condition, flatten};
pub use query::Query;

/// A query or filter that cannot be used, with the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid(pub String);
