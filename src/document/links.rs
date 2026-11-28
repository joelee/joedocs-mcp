//! Wiki links in a Markdown body, found with the parser and options the
//! web interface renders with, so a link the page shows is found and text
//! in a code span or a fenced block never is.

use std::ops::Range;

use pulldown_cmark::{Event, LinkType, Options, Parser, Tag};

/// The parser options of the web interface's renderer.
#[must_use]
pub fn markdown_options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_WIKILINKS
}

/// Where each wiki link and embed in `body` names its page: the byte range
/// of `Page` in `[[Page]]`, `[[Page|label]]`, `[[Page#heading]]`,
/// `[[Page#heading|label]]` and `![[Page]]`, without the spaces around it.
/// A link to a heading of the same page (`[[#heading]]`) has an empty
/// range. In order.
#[must_use]
pub fn wiki_pages(body: &str) -> Vec<Range<usize>> {
    let mut pages = Vec::new();
    for (event, range) in Parser::new_ext(body, markdown_options()).into_offset_iter() {
        let Event::Start(
            Tag::Link {
                link_type: LinkType::WikiLink { .. },
                ..
            }
            | Tag::Image {
                link_type: LinkType::WikiLink { .. },
                ..
            },
        ) = event
        else {
            continue;
        };
        let Some(raw) = body.get(range.clone()) else {
            continue;
        };
        let open = if raw.starts_with("![[") {
            3
        } else if raw.starts_with("[[") {
            2
        } else {
            continue;
        };
        let Some(inner) = raw.strip_suffix("]]").and_then(|rest| rest.get(open..)) else {
            continue;
        };
        let page = &inner[..inner.find(['#', '|']).unwrap_or(inner.len())];
        let start = range.start + open + (page.len() - page.trim_start().len());
        pages.push(start..start + page.trim().len());
    }
    pages
}

/// `text` with each of `ranges` (in order, not overlapping) replaced by
/// `with`.
#[must_use]
pub fn replace_ranges(text: &str, ranges: &[Range<usize>], with: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for range in ranges {
        out.push_str(&text[at..range.start]);
        out.push_str(with);
        at = range.end;
    }
    out.push_str(&text[at..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pages(body: &str) -> Vec<&str> {
        wiki_pages(body).into_iter().map(|r| &body[r]).collect()
    }

    #[test]
    fn every_wiki_link_form_names_its_page() {
        let body = "See [[Old]], [[Old|the old one]], [[ Old #Intro]], \
                    [[Old#Intro|intro]] and ![[Old]].\n\n- [[Other]] [[#Here]]\n";
        assert_eq!(
            pages(body),
            ["Old", "Old", "Old", "Old", "Old", "Other", ""]
        );
    }

    #[test]
    fn links_in_code_are_not_links() {
        let body = "`[[Old]]` and\n\n```\n[[Old]]\n```\n\n    [[Old]]\n\n[[Old]]\n";
        let found = wiki_pages(body);
        assert_eq!(found.len(), 1);
        assert_eq!(&body[found[0].clone()], "Old");
        assert!(body[..found[0].start].ends_with("\n\n[["));
    }

    #[test]
    fn ranges_are_replaced_in_place() {
        let body = "[[Old|x]] and [[Old#h]] but not Old";
        let ranges = wiki_pages(body);
        assert_eq!(
            replace_ranges(body, &ranges, "New name"),
            "[[New name|x]] and [[New name#h]] but not Old"
        );
        assert_eq!(replace_ranges(body, &[], "New"), body);
    }
}
