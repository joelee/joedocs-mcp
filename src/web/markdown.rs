//! Obsidian-style Markdown to safe HTML, on the server.
//!
//! `pulldown-cmark` parses the body (front matter is split off first) with
//! tables, footnotes, strikethrough, task lists and wiki links. The events
//! are then filtered before `pulldown-cmark` writes them out:
//!
//! - raw HTML, block or inline, becomes text, so it is escaped and shown;
//! - links keep only `http`, `https`, `mailto`, relative and `#` targets,
//!   and external ones get `rel="noopener noreferrer"`; images are never
//!   loaded, only linked;
//! - wiki links, embeds and document IDs resolve through a [`Resolver`];
//! - `> [!type] Title` block quotes become callouts, folded with `-`/`+`;
//! - `==highlights==`, `#tags` and `%%comments%%` are handled in text;
//! - headings get prefixed, unique IDs, collected for an "On this page"
//!   list;
//! - Mermaid blocks become placeholders that the page's script draws with
//!   Pele, and `PlantUML` blocks an image from the configured server.
//!
//! Every `Html` event that reaches the writer is written here, from escaped
//! parts; everything else is escaped by `pulldown-cmark`. So the output has
//! no element, attribute or URL scheme this module did not write.

use std::collections::HashMap;

use pulldown_cmark::{
    CodeBlockKind, CowStr, Event, HeadingLevel, LinkType, Options, Parser, Tag, TagEnd,
    TextMergeStream,
};

use super::plantuml;
use crate::document::front_matter;

/// Where a wiki link points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The document.
    pub id: String,
    /// The file of a multi-file document, if not its main file.
    pub file: Option<String>,
}

/// Resolves links against the store, for [`render`].
pub trait Resolver {
    /// The document (or file) a wiki link target names: a title or alias,
    /// or a file of the document being shown.
    fn wiki(&self, target: &str) -> Option<Target>;
    /// Whether `id` is a live document.
    fn document(&self, id: &str) -> bool;
}

/// A heading, for the "On this page" list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heading {
    /// 1 to 6.
    pub level: u8,
    /// Its text.
    pub text: String,
    /// Its `id`.
    pub id: String,
}

/// A rendered document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rendered {
    /// The HTML.
    pub html: String,
    /// The headings, in order.
    pub headings: Vec<Heading>,
    /// Whether it has a Mermaid diagram, so the page loads Pele.
    pub needs_pele: bool,
}

/// Renders a document file (front matter and body) or a body alone.
/// `plantuml_url` is `admin.web_plantuml_url`, or empty.
#[must_use]
pub fn render(markdown: &str, resolver: &dyn Resolver, plantuml_url: &str) -> Rendered {
    let body = front_matter::split(markdown).map_or(markdown, |(_, body)| body);
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_WIKILINKS;
    let events: Vec<Event<'_>> = TextMergeStream::new(Parser::new_ext(body, options)).collect();
    let mut renderer = Renderer {
        resolver,
        plantuml_url: plantuml_url.trim_end_matches('/'),
        out: Vec::with_capacity(events.len()),
        headings: Vec::new(),
        ids: HashMap::new(),
        needs_pele: false,
        closings: Vec::new(),
        quotes: Vec::new(),
    };
    renderer.run(drop_comments(&events).into_iter().map(Item::Md).collect());
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, renderer.out.into_iter());
    Rendered {
        html,
        headings: renderer.headings,
        needs_pele: renderer.needs_pele,
    }
}

/// An event to write: one from the parser, or HTML this module wrote.
/// Keeping them apart means raw HTML from the document can never pass as
/// this module's own.
enum Item<'a> {
    Md(Event<'a>),
    Own(String),
}

/// What a block quote turned into, to close it.
enum Quote {
    Plain,
    Callout { folds: bool },
}

struct Renderer<'r, 'a> {
    resolver: &'r dyn Resolver,
    plantuml_url: &'r str,
    out: Vec<Event<'a>>,
    headings: Vec<Heading>,
    ids: HashMap<String, usize>,
    needs_pele: bool,
    /// How to close each open link: `</a>`, `</span>` or nothing.
    closings: Vec<&'static str>,
    quotes: Vec<Quote>,
}

impl<'a> Renderer<'_, 'a> {
    fn run(&mut self, mut events: Vec<Item<'a>>) {
        let mut i = 0;
        while i < events.len() {
            let item = std::mem::replace(&mut events[i], Item::Md(Event::SoftBreak));
            i += 1;
            let event = match item {
                Item::Own(html) => {
                    self.out.push(Event::Html(html.into()));
                    continue;
                }
                Item::Md(event) => event,
            };
            match event {
                Event::Start(Tag::BlockQuote(_)) => {
                    if let Some(open) = callout(&mut events, i) {
                        self.quotes.push(Quote::Callout { folds: open.folds });
                        self.out.push(Event::Html(open.html.into()));
                    } else {
                        self.quotes.push(Quote::Plain);
                        self.out.push(Event::Start(Tag::BlockQuote(None)));
                    }
                }
                Event::End(TagEnd::BlockQuote(_)) => match self.quotes.pop() {
                    Some(Quote::Callout { folds: true }) => self.html("</div></details>"),
                    Some(Quote::Callout { folds: false }) => self.html("</div></div>"),
                    _ => self.out.push(Event::End(TagEnd::BlockQuote(None))),
                },
                Event::Start(Tag::Heading { level, .. }) => {
                    let text = heading_text(&events[i..]);
                    let id = self.unique_id(&text);
                    self.headings.push(Heading {
                        level: level as u8,
                        text,
                        id: id.clone(),
                    });
                    self.out.push(Event::Start(Tag::Heading {
                        level: demoted(level),
                        id: Some(id.into()),
                        classes: Vec::new(),
                        attrs: Vec::new(),
                    }));
                }
                Event::End(TagEnd::Heading(level)) => {
                    self.out.push(Event::End(TagEnd::Heading(demoted(level))));
                }
                Event::Start(Tag::CodeBlock(kind)) => {
                    let (text, next) =
                        collect_text(&events, i, |e| matches!(e, Event::End(TagEnd::CodeBlock)));
                    i = next;
                    let lang = match kind {
                        CodeBlockKind::Fenced(info) => {
                            info.split_whitespace().next().unwrap_or("").to_owned()
                        }
                        CodeBlockKind::Indented => String::new(),
                    };
                    let block = self.code_block(&lang, &text);
                    self.out.push(Event::Html(block.into()));
                }
                Event::Start(Tag::HtmlBlock) => self.html("<pre class=\"raw-html\">"),
                Event::End(TagEnd::HtmlBlock) => self.html("</pre>"),
                Event::Html(raw) | Event::InlineHtml(raw) => self.out.push(Event::Text(raw)),
                Event::Start(Tag::Table(alignments)) => {
                    self.html("<div class=\"table-wrap\">");
                    self.out.push(Event::Start(Tag::Table(alignments)));
                }
                Event::End(TagEnd::Table) => {
                    self.out.push(Event::End(TagEnd::Table));
                    self.html("</div>");
                }
                Event::Start(Tag::Link {
                    link_type,
                    dest_url,
                    title,
                    ..
                }) => self.open_link(link_type, &dest_url, &title),
                Event::End(TagEnd::Link) => {
                    let closing = self.closings.pop().unwrap_or("");
                    if !closing.is_empty() {
                        self.html(closing);
                    }
                }
                Event::Start(Tag::Image {
                    link_type,
                    dest_url,
                    ..
                }) => {
                    let (alt, next) =
                        collect_text(&events, i, |e| matches!(e, Event::End(TagEnd::Image)));
                    i = next;
                    let image = self.image(link_type, &dest_url, &alt);
                    self.out.push(Event::Html(image.into()));
                }
                Event::Text(text) => {
                    let parts = self.text(&text);
                    self.out.extend(parts);
                }
                other => self.out.push(other),
            }
        }
    }

    fn html(&mut self, html: &'static str) {
        self.out.push(Event::Html(CowStr::Borrowed(html)));
    }

    /// `h-<slug>`, made unique on the page.
    fn unique_id(&mut self, text: &str) -> String {
        let base = format!("h-{}", slug(text));
        let seen = self.ids.entry(base.clone()).or_insert(0);
        *seen += 1;
        if *seen == 1 {
            base
        } else {
            format!("{base}-{seen}")
        }
    }

    fn open_link(&mut self, link_type: LinkType, dest: &str, title: &str) {
        if matches!(link_type, LinkType::WikiLink { .. }) {
            if let Some(href) = self.wiki_href(dest) {
                let open = format!("<a class=\"wikilink\" href=\"{}\">", escape(&href));
                self.out.push(Event::Html(open.into()));
                self.closings.push("</a>");
            } else {
                self.html(
                    "<span class=\"wikilink unresolved\" title=\"No document with this title\">",
                );
                self.closings.push("</span>");
            }
            return;
        }
        match safe_href(dest) {
            Some(external) => {
                let rel = if external {
                    " rel=\"noopener noreferrer\""
                } else {
                    ""
                };
                let title = if title.is_empty() {
                    String::new()
                } else {
                    format!(" title=\"{}\"", escape(title))
                };
                let open = format!("<a href=\"{}\"{rel}{title}>", escape(dest));
                self.out.push(Event::Html(open.into()));
                self.closings.push("</a>");
            }
            None => self.closings.push(""),
        }
    }

    /// The page a wiki link target opens: `[[Title]]`, `[[Title#Heading]]`,
    /// `[[#Heading]]` or a file of the current document.
    fn wiki_href(&self, dest: &str) -> Option<String> {
        let (page, heading) = match dest.split_once('#') {
            Some((page, heading)) => (page.trim(), Some(heading.trim())),
            None => (dest.trim(), None),
        };
        let anchor = heading
            .filter(|h| !h.is_empty())
            .map(|h| format!("#h-{}", slug(h)))
            .unwrap_or_default();
        if page.is_empty() {
            return (!anchor.is_empty()).then_some(anchor);
        }
        let target = self.resolver.wiki(page)?;
        let mut href = format!("/admin/documents/{}", target.id);
        if let Some(file) = target.file {
            href.push_str("?file=");
            href.extend(form_urlencoded::byte_serialize(file.as_bytes()));
        }
        href.push_str(&anchor);
        Some(href)
    }

    fn image(&self, link_type: LinkType, dest: &str, alt: &str) -> String {
        if matches!(link_type, LinkType::WikiLink { .. }) {
            let label = format!(
                "Embedded: {}",
                escape(if alt.is_empty() { dest } else { alt })
            );
            return match self.wiki_href(dest) {
                Some(href) => format!(
                    "<a class=\"wikilink embed\" href=\"{}\">{label}</a>",
                    escape(&href)
                ),
                None => format!(
                    "<span class=\"wikilink embed unresolved\" title=\"No document with this title\">{label}</span>"
                ),
            };
        }
        let label = format!("Image: {}", escape(if alt.is_empty() { dest } else { alt }));
        match safe_href(dest) {
            Some(external) => {
                let rel = if external {
                    " rel=\"noopener noreferrer\""
                } else {
                    ""
                };
                format!(
                    "<a class=\"image-link\" href=\"{}\"{rel}>{label}</a>",
                    escape(dest)
                )
            }
            None => format!(
                "<span class=\"image-link\">{}</span>",
                escape(&format!("Image: {alt}"))
            ),
        }
    }

    fn code_block(&mut self, lang: &str, text: &str) -> String {
        let source = format!(
            "<pre class=\"diagram-source\"><code>{}</code></pre>",
            escape(text)
        );
        match lang.to_ascii_lowercase().as_str() {
            "mermaid" => {
                self.needs_pele = true;
                format!("<div class=\"diagram mermaid\" data-diagram=\"mermaid\">{source}</div>")
            }
            "plantuml" | "puml" => {
                let encoded = (!self.plantuml_url.is_empty())
                    .then(|| plantuml::encode(text))
                    .flatten();
                match encoded {
                    Some(encoded) => format!(
                        "<figure class=\"diagram plantuml\"><img src=\"{}/svg/{encoded}\" alt=\"PlantUML diagram\" loading=\"lazy\"><details><summary>Show source</summary>{source}</details></figure>",
                        escape(self.plantuml_url)
                    ),
                    None => format!(
                        "<figure class=\"diagram plantuml\"><figcaption>PlantUML diagram (source)</figcaption>{source}</figure>"
                    ),
                }
            }
            _ => {
                let label = if lang.is_empty() {
                    String::new()
                } else {
                    format!("<span class=\"code-lang\">{}</span>", escape(lang))
                };
                format!(
                    "<figure class=\"code\">{label}<pre><code>{}</code></pre></figure>",
                    escape(text)
                )
            }
        }
    }

    /// Text outside code: highlights everywhere, and tags and document IDs
    /// outside links.
    fn text(&self, text: &str) -> Vec<Event<'a>> {
        let mut parts = Vec::new();
        let mut rest = text;
        while let Some(start) = rest.find("==") {
            let after = &rest[start + 2..];
            match after.find("==") {
                Some(end) if end > 0 => {
                    self.words(&rest[..start], &mut parts);
                    parts.push(Event::Html(CowStr::Borrowed("<mark>")));
                    self.words(&after[..end], &mut parts);
                    parts.push(Event::Html(CowStr::Borrowed("</mark>")));
                    rest = &after[end + 2..];
                }
                _ => break,
            }
        }
        self.words(rest, &mut parts);
        parts
    }

    /// `text` with `#tags` as chips and known document IDs as links.
    fn words(&self, text: &str, parts: &mut Vec<Event<'a>>) {
        if text.is_empty() {
            return;
        }
        if !self.closings.is_empty() {
            parts.push(Event::Text(text.to_owned().into()));
            return;
        }
        let mut plain = String::new();
        let mut chars = text.char_indices().peekable();
        let mut previous: Option<char> = None;
        while let Some((at, c)) = chars.next() {
            let boundary = previous.is_none_or(|p| !p.is_alphanumeric() && p != '_');
            let token = if c == '#' && previous.is_none_or(char::is_whitespace) {
                tag_at(&text[at..]).map(|tag| (tag, true))
            } else if c.is_ascii_uppercase() && boundary {
                document_id_at(&text[at..])
                    .filter(|id| self.resolver.document(id))
                    .map(|id| (id, false))
            } else {
                None
            };
            if let Some((token, is_tag)) = token {
                if !plain.is_empty() {
                    parts.push(Event::Text(std::mem::take(&mut plain).into()));
                }
                let html = if is_tag {
                    format!("<span class=\"chip tag\">{}</span>", escape(token))
                } else {
                    format!(
                        "<a class=\"docid\" href=\"/admin/documents/{0}\">{0}</a>",
                        escape(token)
                    )
                };
                parts.push(Event::Html(html.into()));
                let end = at + token.len();
                while chars.peek().is_some_and(|(i, _)| *i < end) {
                    chars.next();
                }
                previous = token.chars().last();
                continue;
            }
            plain.push(c);
            previous = Some(c);
        }
        if !plain.is_empty() {
            parts.push(Event::Text(plain.into()));
        }
    }
}

/// An opening callout: its HTML and whether it folds.
struct CalloutOpen {
    html: String,
    folds: bool,
}

/// If the block quote whose content starts at `events[i]` is a callout
/// (`> [!type]...`), rewrites its first line in place and returns how to
/// open it.
fn callout(events: &mut Vec<Item<'_>>, i: usize) -> Option<CalloutOpen> {
    if !matches!(events.get(i), Some(Item::Md(Event::Start(Tag::Paragraph)))) {
        return None;
    }
    let Some(Item::Md(Event::Text(first))) = events.get(i + 1) else {
        return None;
    };
    let rest = first.strip_prefix("[!")?;
    let (kind, after) = rest.split_once(']')?;
    if kind.is_empty()
        || !kind
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let (fold, title) = match after.chars().next() {
        Some('-') => (Some(false), &after[1..]),
        Some('+') => (Some(true), &after[1..]),
        _ => (None, after),
    };
    let title = title.trim_start().to_owned();
    let class = callout_class(kind);
    // The title runs to the end of the first line.
    let end = events[i + 2..]
        .iter()
        .position(|e| {
            matches!(
                e,
                Item::Md(Event::SoftBreak | Event::HardBreak | Event::End(TagEnd::Paragraph))
            )
        })
        .map(|p| p + i + 2)?;
    let empty_title = title.is_empty() && end == i + 2;
    let title_event = if empty_title {
        let mut name = kind.to_ascii_lowercase();
        if let Some(first) = name.get_mut(..1) {
            first.make_ascii_uppercase();
        }
        Event::Text(name.into())
    } else {
        Event::Text(title.into())
    };
    let (open, title_open, title_close) = match fold {
        None => (
            format!("<div class=\"callout callout-{class}\">"),
            "<div class=\"callout-title\">",
            "</div><div class=\"callout-body\">",
        ),
        Some(open) => (
            format!(
                "<details class=\"callout callout-{class}\"{}>",
                if open { " open" } else { "" }
            ),
            "<summary class=\"callout-title\">",
            "</summary><div class=\"callout-body\">",
        ),
    };
    // After the title: the rest of the paragraph, if any, opens a new one.
    let after_title = match events[end] {
        Item::Md(Event::End(TagEnd::Paragraph)) => vec![Item::Own(title_close.to_owned())],
        _ => vec![
            Item::Own(title_close.to_owned()),
            Item::Md(Event::Start(Tag::Paragraph)),
        ],
    };
    events.splice(end..=end, after_title);
    events.splice(
        i..=i + 1,
        [Item::Own(title_open.to_owned()), Item::Md(title_event)],
    );
    Some(CalloutOpen {
        html: open,
        folds: fold.is_some(),
    })
}

/// The class of a callout type: Obsidian's types and aliases, and `note`
/// for any other.
fn callout_class(kind: &str) -> &'static str {
    match kind.to_ascii_lowercase().as_str() {
        "abstract" | "summary" | "tldr" => "abstract",
        "info" => "info",
        "todo" => "todo",
        "tip" | "hint" | "important" => "tip",
        "success" | "check" | "done" => "success",
        "question" | "help" | "faq" => "question",
        "warning" | "caution" | "attention" => "warning",
        "failure" | "fail" | "missing" => "failure",
        "danger" | "error" => "danger",
        "bug" => "bug",
        "example" => "example",
        "quote" | "cite" => "quote",
        _ => "note",
    }
}

/// A document heading one level down, so the page title stays the page's
/// only `<h1>`; `######` stays an `<h6>`.
fn demoted(level: HeadingLevel) -> HeadingLevel {
    match level {
        HeadingLevel::H1 => HeadingLevel::H2,
        HeadingLevel::H2 => HeadingLevel::H3,
        HeadingLevel::H3 => HeadingLevel::H4,
        HeadingLevel::H4 => HeadingLevel::H5,
        HeadingLevel::H5 | HeadingLevel::H6 => HeadingLevel::H6,
    }
}

/// The text of the heading that starts at `events[0]`.
fn heading_text(events: &[Item<'_>]) -> String {
    let mut text = String::new();
    for event in events {
        match event {
            Item::Md(Event::End(TagEnd::Heading(_))) => break,
            Item::Md(Event::Text(t) | Event::Code(t)) => text.push_str(t),
            _ => {}
        }
    }
    text.trim().to_owned()
}

/// The text inside a container from `events[i]` up to the event `is_end`
/// matches, and the index after it.
fn collect_text(
    events: &[Item<'_>],
    i: usize,
    is_end: impl Fn(&Event<'_>) -> bool,
) -> (String, usize) {
    let mut text = String::new();
    let mut j = i;
    while let Some(item) = events.get(j) {
        j += 1;
        let Item::Md(event) = item else { continue };
        if is_end(event) {
            break;
        }
        if let Event::Text(t) | Event::Code(t) = event {
            text.push_str(t);
        }
    }
    (text, j)
}

/// `Some(external)` for a target a link may use: `http`, `https` and
/// `mailto` (external), and relative or `#` targets; `None` for any other
/// scheme. Whitespace and control characters, which browsers drop from a
/// URL, are ignored when finding the scheme.
fn safe_href(url: &str) -> Option<bool> {
    let compact: String = url
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect::<String>()
        .to_ascii_lowercase();
    if compact.starts_with("//") {
        return Some(true);
    }
    match compact.find(':') {
        Some(colon) if !compact[..colon].contains(['/', '?', '#']) => {
            matches!(&compact[..colon], "http" | "https" | "mailto").then_some(true)
        }
        _ => Some(false),
    }
}

/// A `#tag` at the start of `text`: letters, digits, `_`, `-` and `/`, not
/// only digits.
fn tag_at(text: &str) -> Option<&str> {
    let rest = text.strip_prefix('#')?;
    let len: usize = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '/'))
        .map(char::len_utf8)
        .sum();
    let name = &rest[..len];
    (!name.is_empty() && !name.chars().all(|c| c.is_ascii_digit())).then(|| &text[..=len])
}

/// A document ID (`KEY-123`) at the start of `text`, ending at a word
/// boundary.
fn document_id_at(text: &str) -> Option<&str> {
    let key_len = text
        .bytes()
        .take_while(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        .count();
    if key_len == 0 || key_len > crate::document::MAX_KEY_LEN {
        return None;
    }
    let rest = text[key_len..].strip_prefix('-')?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let end = key_len + 1 + digits;
    let next = text[end..].chars().next();
    if next.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-') {
        return None;
    }
    Some(&text[..end])
}

/// A heading's slug: lower case, letters and digits kept, runs of other
/// characters as one `-`.
fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
        } else if (c.is_whitespace() || c == '-' || c == '_') && !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "section".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// `text` escaped for HTML text and double-quoted attributes.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Removes `%%comments%%` that open and close within one paragraph (or
/// heading, list item or table cell), outside code. A comment that does not
/// close there, or would cut formatting in half, stays as written.
fn drop_comments<'a>(events: &[Event<'a>]) -> Vec<Event<'a>> {
    let mut out = Vec::with_capacity(events.len());
    let mut in_code = false;
    let mut i = 0;
    while i < events.len() {
        match &events[i] {
            Event::Start(Tag::CodeBlock(_)) => in_code = true,
            Event::End(TagEnd::CodeBlock) => in_code = false,
            Event::Text(text) if !in_code && text.contains("%%") => {
                let (replaced, next) = comment_span(events, i);
                out.extend(replaced);
                i = next;
                continue;
            }
            _ => {}
        }
        out.push(events[i].clone());
        i += 1;
    }
    out
}

/// The events that replace the text at `events[i]` with its comments
/// removed, and the index to go on from.
fn comment_span<'a>(events: &[Event<'a>], i: usize) -> (Vec<Event<'a>>, usize) {
    let Event::Text(text) = &events[i] else {
        return (vec![events[i].clone()], i + 1);
    };
    let mut kept = String::new();
    let mut rest: &str = text;
    while let Some(open) = rest.find("%%") {
        let after = &rest[open + 2..];
        if let Some(close) = after.find("%%") {
            kept.push_str(&rest[..open]);
            rest = &after[close + 2..];
            continue;
        }
        // Unclosed here: look for the end in a later text of the same run.
        if let Some((j, tail)) = closing_in_run(events, i + 1) {
            kept.push_str(&rest[..open]);
            return (
                vec![Event::Text(kept.into()), Event::Text(tail.into())],
                j + 1,
            );
        }
        break;
    }
    kept.push_str(rest);
    (vec![Event::Text(kept.into())], i + 1)
}

/// The first text after `events[start..]`, within the same run of inline
/// events and with its formatting balanced, that closes a comment, and what
/// follows the `%%` in it.
fn closing_in_run(events: &[Event<'_>], start: usize) -> Option<(usize, String)> {
    let mut depth = 0_i32;
    for (j, event) in events.iter().enumerate().skip(start) {
        match event {
            Event::Text(t) => {
                if let Some(close) = t.find("%%")
                    && depth == 0
                {
                    return Some((j, t[close + 2..].to_owned()));
                }
            }
            Event::Code(_)
            | Event::SoftBreak
            | Event::HardBreak
            | Event::InlineHtml(_)
            | Event::FootnoteReference(_) => {}
            Event::Start(Tag::Emphasis | Tag::Strong | Tag::Strikethrough | Tag::Link { .. }) => {
                depth += 1;
            }
            Event::End(
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link,
            ) => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
            }
            _ => return None,
        }
    }
    None
}

#[cfg(test)]
#[path = "markdown_tests.rs"]
mod tests;
