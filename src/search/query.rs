//! The forgiving text query grammar and its FTS5 translation.
//!
//! A query is a list of terms. A term is a bare word or a `"quoted
//! phrase"`, optionally starting with `-` (exclude it) and ending with `*`
//! (match it as a prefix). Terms must all match, except that terms joined
//! by an upper-case `OR` match if any of them does. Every term is quoted
//! when translated, so no input can produce an invalid FTS5 expression:
//! an unclosed quote runs to the end of the query, and terms without a
//! letter or digit are ignored.

use super::Invalid;

/// Longest query accepted, in characters.
pub const MAX_QUERY_CHARS: usize = 1000;

/// Most terms in one query.
pub const MAX_TERMS: usize = 64;

/// One word or phrase.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Term {
    text: String,
    prefix: bool,
}

impl Term {
    fn to_fts5(&self) -> String {
        let star = if self.prefix { "*" } else { "" };
        format!("\"{}\"{star}", self.text.replace('"', "\"\""))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Term { term: Term, excluded: bool },
    Or,
}

/// A parsed text query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    /// Every group must match; a group matches if any of its terms does.
    groups: Vec<Vec<Term>>,
    /// No document matching one of these is returned.
    excluded: Vec<Term>,
}

impl Query {
    /// Parses `input`. Returns `None` when there is nothing to search for
    /// (empty or only punctuation).
    ///
    /// # Errors
    ///
    /// Returns [`Invalid`] when the query is too long, has too many terms,
    /// or only excludes.
    pub fn parse(input: &str) -> Result<Option<Self>, Invalid> {
        if input.chars().count() > MAX_QUERY_CHARS {
            return Err(Invalid(format!(
                "must be at most {MAX_QUERY_CHARS} characters"
            )));
        }
        let tokens = tokenize(input);
        let terms = tokens
            .iter()
            .filter(|t| matches!(t, Token::Term { .. }))
            .count();
        if terms > MAX_TERMS {
            return Err(Invalid(format!("must have at most {MAX_TERMS} terms")));
        }
        let mut query = Self {
            groups: Vec::new(),
            excluded: Vec::new(),
        };
        let mut joined = false;
        for token in tokens {
            match token {
                Token::Or => joined = !query.groups.is_empty(),
                Token::Term {
                    term,
                    excluded: true,
                } => {
                    query.excluded.push(term);
                    joined = false;
                }
                Token::Term { term, .. } => {
                    match query.groups.last_mut() {
                        Some(group) if joined => group.push(term),
                        _ => query.groups.push(vec![term]),
                    }
                    joined = false;
                }
            }
        }
        if query.groups.is_empty() {
            if query.excluded.is_empty() {
                return Ok(None);
            }
            return Err(Invalid(
                "needs at least one word to find; -word only excludes".to_owned(),
            ));
        }
        Ok(Some(query))
    }

    /// The FTS5 `MATCH` expression.
    #[must_use]
    pub fn to_fts5(&self) -> String {
        let groups: Vec<String> = self
            .groups
            .iter()
            .map(|group| {
                let terms: Vec<String> = group.iter().map(Term::to_fts5).collect();
                if terms.len() == 1 {
                    terms.join("")
                } else {
                    format!("({})", terms.join(" OR "))
                }
            })
            .collect();
        let wanted = groups.join(" AND ");
        if self.excluded.is_empty() {
            return wanted;
        }
        let excluded: Vec<String> = self.excluded.iter().map(Term::to_fts5).collect();
        format!("({wanted}) NOT ({})", excluded.join(" OR "))
    }
}

fn tokenize(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        let excluded = c == '-';
        if excluded {
            chars.next();
        }
        let (mut text, quoted) = if chars.peek() == Some(&'"') {
            chars.next();
            let text: String = chars.by_ref().take_while(|&c| c != '"').collect();
            (text, true)
        } else {
            let mut text = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() || c == '"' {
                    break;
                }
                text.push(c);
                chars.next();
            }
            (text, false)
        };
        let prefix = if quoted {
            chars.next_if_eq(&'*').is_some()
        } else {
            let trimmed = text.trim_end_matches('*').len();
            let prefix = trimmed < text.len();
            text.truncate(trimmed);
            prefix
        };
        if !quoted && !excluded && !prefix && text == "OR" {
            tokens.push(Token::Or);
        } else if text.chars().any(char::is_alphanumeric) {
            tokens.push(Token::Term {
                term: Term { text, prefix },
                excluded,
            });
        }
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fts(input: &str) -> Option<String> {
        Query::parse(input).unwrap().map(|q| q.to_fts5())
    }

    #[test]
    fn words_are_quoted_and_all_required() {
        assert_eq!(fts("rate limit").unwrap(), r#""rate" AND "limit""#);
        assert_eq!(fts("  single ").unwrap(), r#""single""#);
    }

    #[test]
    fn phrases_prefixes_and_exclusions_translate() {
        assert_eq!(
            fts(r#""rate limit" retry -redis auth*"#).unwrap(),
            r#"("rate limit" AND "retry" AND "auth"*) NOT ("redis")"#
        );
        assert_eq!(fts(r#""rate lim"*"#).unwrap(), r#""rate lim"*"#);
        assert_eq!(
            fts(r#"a -"two words" -b"#).unwrap(),
            r#"("a") NOT ("two words" OR "b")"#
        );
    }

    #[test]
    fn or_joins_neighbouring_terms() {
        assert_eq!(
            fts("redis OR valkey cache").unwrap(),
            r#"("redis" OR "valkey") AND "cache""#
        );
        assert_eq!(fts("a OR b OR c").unwrap(), r#"("a" OR "b" OR "c")"#);
        // A leading, trailing or doubled OR is ignored, and lower-case "or"
        // is an ordinary word.
        assert_eq!(fts("OR a OR").unwrap(), r#""a""#);
        assert_eq!(fts("a OR OR b").unwrap(), r#"("a" OR "b")"#);
        assert_eq!(fts("a or b").unwrap(), r#""a" AND "or" AND "b""#);
        assert_eq!(fts(r#""OR""#).unwrap(), r#""OR""#);
        // An exclusion breaks an OR chain.
        assert_eq!(fts("a OR -b c").unwrap(), r#"("a" AND "c") NOT ("b")"#);
    }

    #[test]
    fn hostile_input_still_translates_safely() {
        assert_eq!(fts(r#"say "hello"#).unwrap(), r#""say" AND "hello""#);
        assert_eq!(fts(r#"a"b"#).unwrap(), r#""a" AND "b""#);
        assert_eq!(
            fts(r"NEAR(a b) col:x ^y").unwrap(),
            r#""NEAR(a" AND "b)" AND "col:x" AND "^y""#
        );
        assert_eq!(fts(r#""""#), None);
        assert_eq!(fts("- * -- ** ..."), None);
        assert_eq!(fts(""), None);
        assert_eq!(fts("a**").unwrap(), r#""a"*"#);
    }

    #[test]
    fn unusable_queries_are_refused() {
        let err = Query::parse("-redis").unwrap_err();
        assert!(err.0.contains("at least one word"), "{err:?}");
        let long = "a".repeat(MAX_QUERY_CHARS + 1);
        assert!(Query::parse(&long).unwrap_err().0.contains("at most"));
        let many = vec!["w"; MAX_TERMS + 1].join(" ");
        assert!(Query::parse(&many).unwrap_err().0.contains("terms"));
    }
}
