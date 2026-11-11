//! Tests for the Markdown renderer: one per row of the feature table in
//! JD-32 section 8, and hostile fixtures that must not produce active
//! content.

use super::*;
use crate::web::plantuml::tests::long_diagram;

/// A resolver over a fixed set of documents.
struct Fixed;

impl Resolver for Fixed {
    fn wiki(&self, target: &str) -> Option<Target> {
        match target.to_lowercase().as_str() {
            "other plan" | "the alias" => Some(Target {
                id: "JD-2".into(),
                file: None,
            }),
            "items/one" | "one" => Some(Target {
                id: "JD-5".into(),
                file: Some("items/one".into()),
            }),
            _ => None,
        }
    }

    fn document(&self, id: &str) -> bool {
        matches!(id, "JD-1" | "JD-2" | "NT-14")
    }
}

fn html(markdown: &str) -> String {
    render(markdown, &Fixed, "").html
}

/// Every real tag in `html` (escaped text has no `<`).
fn tags(html: &str) -> Vec<&str> {
    html.split('<')
        .skip(1)
        .filter_map(|rest| rest.split_once('>').map(|(tag, _)| tag))
        .collect()
}

/// The attributes of a tag, as `(name, value)`, honouring double quotes.
fn attributes(tag: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = tag.split_once(char::is_whitespace).map_or("", |(_, r)| r);
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            return out;
        }
        let name_len = rest
            .find(|c: char| c == '=' || c.is_whitespace())
            .unwrap_or(rest.len());
        let name = rest[..name_len].to_ascii_lowercase();
        rest = &rest[name_len..];
        let mut value = String::new();
        if let Some(after) = rest.strip_prefix('=') {
            if let Some(quoted) = after.strip_prefix('"') {
                let end = quoted.find('"').expect("closed quote");
                value = quoted[..end].to_owned();
                rest = &quoted[end + 1..];
            } else {
                let end = after.find(char::is_whitespace).unwrap_or(after.len());
                value = after[..end].to_owned();
                rest = &after[end..];
            }
        }
        out.push((name, value));
    }
}

/// Whether `html` holds no active content.
fn assert_inert(html: &str) {
    for tag in tags(html) {
        let lower = tag.to_ascii_lowercase();
        let name = lower.split_whitespace().next().unwrap_or("");
        for banned in [
            "script", "iframe", "style", "object", "embed", "form", "base", "meta", "link", "svg",
        ] {
            assert_ne!(name.trim_start_matches('/'), banned, "<{tag}> in {html}");
        }
        assert_ne!(name, "img", "<{tag}> in {html}");
        for (attribute, value) in attributes(tag) {
            assert!(
                !attribute.starts_with("on"),
                "event attribute in <{tag}>: {html}"
            );
            assert_ne!(attribute, "style", "style in <{tag}>");
            assert!(
                !attribute.contains(['"', '<', '\'']),
                "a broken attribute in <{tag}>: {html}"
            );
            if matches!(attribute.as_str(), "href" | "src") {
                let value = value.to_ascii_lowercase();
                for scheme in ["javascript:", "data:", "vbscript:"] {
                    assert!(!value.contains(scheme), "{scheme} in <{tag}>: {html}");
                }
            }
        }
    }
}

#[test]
fn front_matter_is_not_rendered_as_text() {
    let rendered = render(
        "---\ntitle: Secret title\nstatus: draft\n---\n# Hello\n",
        &Fixed,
        "",
    );
    assert!(!rendered.html.contains("Secret title"), "{}", rendered.html);
    assert!(rendered.html.contains("Hello"));
    // A file without front matter renders whole.
    assert!(html("Plain *text*").contains("<em>text</em>"));
}

#[test]
fn wiki_links_resolve_by_title_alias_and_file() {
    let out = html("See [[Other plan]], [[the alias|that one]] and [[Other plan#Next steps]].");
    assert!(
        out.contains("<a class=\"wikilink\" href=\"/admin/documents/JD-2\">Other plan</a>"),
        "{out}"
    );
    assert!(
        out.contains("href=\"/admin/documents/JD-2\">that one</a>"),
        "{out}"
    );
    assert!(
        out.contains("href=\"/admin/documents/JD-2#h-next-steps\""),
        "{out}"
    );
    let file = html("[[items/one]]");
    assert!(
        file.contains("href=\"/admin/documents/JD-5?file=items%2Fone\""),
        "{file}"
    );
    let same = html("[[#Scope]]");
    assert!(same.contains("href=\"#h-scope\""), "{same}");
    let missing = html("[[No such page]]");
    assert!(
        missing.contains("class=\"wikilink unresolved\""),
        "{missing}"
    );
    assert!(missing.contains("No document with this title"), "{missing}");
    assert!(!missing.contains("<a"), "{missing}");
}

#[test]
fn embeds_are_links_not_transclusions() {
    let out = html("![[Other plan]]");
    assert!(out.contains("class=\"wikilink embed\""), "{out}");
    assert!(out.contains("href=\"/admin/documents/JD-2\""), "{out}");
    assert!(out.contains("Embedded"), "{out}");
}

#[test]
fn document_ids_link_when_they_exist() {
    let out = html("Follows JD-1 and NT-14, not XX-9 or JD-1x or aJD-1.");
    assert!(
        out.contains("<a class=\"docid\" href=\"/admin/documents/JD-1\">JD-1</a>"),
        "{out}"
    );
    assert!(out.contains("href=\"/admin/documents/NT-14\""), "{out}");
    assert!(!out.contains("XX-9</a>"), "{out}");
    assert!(!out.contains("JD-1x</a>"), "{out}");
    assert_eq!(out.matches("class=\"docid\"").count(), 2, "{out}");
    let code = html("`JD-1` and\n\n    JD-2\n");
    assert!(!code.contains("docid"), "{code}");
}

#[test]
fn callouts_get_their_type_title_and_folding() {
    let out = html("> [!warning] Mind the gap\n> Body text.\n");
    assert!(out.contains("class=\"callout callout-warning\""), "{out}");
    assert!(
        out.contains("<div class=\"callout-title\">Mind the gap</div>"),
        "{out}"
    );
    assert!(out.contains("Body text."), "{out}");
    assert!(!out.contains("[!warning]"), "{out}");
    let folded = html("> [!tip]- Closed\n> Hidden.\n");
    assert!(
        folded.contains("<details class=\"callout callout-tip\">"),
        "{folded}"
    );
    assert!(
        folded.contains("<summary class=\"callout-title\">Closed</summary>"),
        "{folded}"
    );
    let open = html("> [!info]+ Open\n> Shown.\n");
    assert!(
        open.contains("<details class=\"callout callout-info\" open>"),
        "{open}"
    );
    let alias = html("> [!TLDR]\n> Short.\n");
    assert!(alias.contains("callout-abstract"), "{alias}");
    assert!(
        alias.contains(">Tldr<"),
        "default title from the type: {alias}"
    );
    let unknown = html("> [!banana] Odd\n> Text\n");
    assert!(unknown.contains("callout-note"), "{unknown}");
    let quote = html("> Just a quote.\n");
    assert!(quote.contains("<blockquote>"), "{quote}");
    let nested = html("> [!note] Outer\n> > [!bug] Inner\n> > text\n");
    assert!(
        nested.contains("callout-note") && nested.contains("callout-bug"),
        "{nested}"
    );
    assert_eq!(
        nested.matches("<div class=\"callout callout-").count(),
        2,
        "{nested}"
    );
    assert_eq!(nested.matches("</div></div>").count(), 2, "{nested}");
}

#[test]
fn tables_task_lists_strikethrough_and_footnotes() {
    let out = html(
        "| a | b |\n|---|---|\n| 1 | 2 |\n\n- [x] done\n- [ ] todo\n\n~~old~~ and a note[^1].\n\n[^1]: The note.\n",
    );
    assert!(out.contains("<div class=\"table-wrap\"><table>"), "{out}");
    assert!(out.contains("</table>\n</div>"), "{out}");
    assert!(out.contains("type=\"checkbox\""), "{out}");
    assert!(out.contains("disabled"), "{out}");
    assert!(out.contains("<del>old</del>"), "{out}");
    assert!(out.contains("footnote"), "{out}");
}

#[test]
fn highlights_tags_and_comments() {
    let out = html("Some ==important== words with #release/next and #todo, not #123 or a#b.");
    assert!(out.contains("<mark>important</mark>"), "{out}");
    assert!(
        out.contains("<span class=\"chip tag\">#release/next</span>"),
        "{out}"
    );
    assert!(
        out.contains("<span class=\"chip tag\">#todo</span>"),
        "{out}"
    );
    assert!(!out.contains(">#123<"), "{out}");
    assert!(!out.contains(">#b<"), "{out}");
    let comments = html("Before %%hidden **note**%% after.\n\n`%%kept%%`\n");
    assert!(!comments.contains("hidden"), "{comments}");
    assert!(comments.contains("Before  after."), "{comments}");
    assert!(comments.contains("%%kept%%"), "{comments}");
    // A comment that does not close in its paragraph stays visible.
    let open = html("An %%open comment\n\nnext paragraph %% here");
    assert!(open.contains("open comment"), "{open}");
}

#[test]
fn headings_get_prefixed_unique_ids_and_a_contents_list() {
    let rendered = render(
        "# Intro\n## Next steps\n## Next steps\n### Code `x`\n",
        &Fixed,
        "",
    );
    assert!(
        rendered.html.contains("<h1 id=\"h-intro\">"),
        "{}",
        rendered.html
    );
    assert!(rendered.html.contains("<h2 id=\"h-next-steps\">"));
    assert!(rendered.html.contains("<h2 id=\"h-next-steps-2\">"));
    let ids: Vec<_> = rendered.headings.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(
        ids,
        ["h-intro", "h-next-steps", "h-next-steps-2", "h-code-x"]
    );
    assert_eq!(rendered.headings[3].level, 3);
    assert_eq!(rendered.headings[3].text, "Code x");
}

#[test]
fn raw_html_is_escaped_and_shown() {
    let out =
        html("<div onclick=\"x()\">block</div>\n\ninline <b>bold</b> <script>alert(1)</script>\n");
    assert!(out.contains("&lt;div onclick="), "{out}");
    assert!(out.contains("&lt;b&gt;bold&lt;/b&gt;"), "{out}");
    assert!(out.contains("&lt;script&gt;"), "{out}");
    assert_inert(&out);
}

#[test]
fn only_safe_link_schemes_are_links() {
    let out = html(
        "[web](https://example.net/a?b=1) [mail](mailto:a@example.net) [rel](other.md) [anchor](#top) [bad](javascript:alert(1)) [ftp](ftp://x)",
    );
    assert!(
        out.contains("<a href=\"https://example.net/a?b=1\" rel=\"noopener noreferrer\">web</a>"),
        "{out}"
    );
    assert!(out.contains("href=\"mailto:a@example.net\""), "{out}");
    assert!(out.contains("<a href=\"other.md\">rel</a>"), "{out}");
    assert!(out.contains("<a href=\"#top\">anchor</a>"), "{out}");
    assert!(out.contains("bad"), "{out}");
    assert!(!out.contains("javascript:"), "{out}");
    assert!(!out.contains("ftp://"), "{out}");
}

#[test]
fn images_are_links_and_never_loaded() {
    let out =
        html("![a chart](https://example.net/chart.png) ![tracker](data:image/png;base64,AAAA)");
    assert!(!out.contains("<img"), "{out}");
    assert!(
        out.contains("href=\"https://example.net/chart.png\""),
        "{out}"
    );
    assert!(out.contains("Image: a chart"), "{out}");
    assert!(out.contains("tracker"), "{out}");
    assert!(!out.contains("data:"), "{out}");
}

#[test]
fn code_blocks_are_escaped_with_their_language() {
    let out = html("```rust ignore\nfn x() { \"<b>\" }\n```\n");
    assert!(
        out.contains("<span class=\"code-lang\">rust</span>"),
        "{out}"
    );
    assert!(out.contains("&lt;b&gt;"), "{out}");
    let plain = html("```\nno language\n```\n");
    assert!(!plain.contains("code-lang"), "{plain}");
}

#[test]
fn mermaid_blocks_become_diagram_placeholders() {
    let rendered = render("```mermaid\nflowchart LR\n  A --> B\n```\n", &Fixed, "");
    assert!(rendered.needs_pele);
    assert!(
        rendered.html.contains("class=\"diagram mermaid\""),
        "{}",
        rendered.html
    );
    assert!(rendered.html.contains("A --&gt; B"), "{}", rendered.html);
    assert!(!render("plain", &Fixed, "").needs_pele);
}

#[test]
fn plantuml_blocks_are_images_only_with_a_server() {
    let source = "```plantuml\n@startuml\nBob -> Alice : hello\n@enduml\n```\n";
    let without = render(source, &Fixed, "");
    assert!(!without.html.contains("<img"), "{}", without.html);
    assert!(without.html.contains("PlantUML"), "{}", without.html);
    assert!(without.html.contains("Bob -&gt; Alice"), "{}", without.html);
    let with = render(source, &Fixed, "https://plantuml.example.net/plantuml");
    let img = tags(&with.html)
        .into_iter()
        .find(|t| t.starts_with("img "))
        .unwrap_or_else(|| panic!("no image: {}", with.html));
    assert!(
        img.contains("src=\"https://plantuml.example.net/plantuml/svg/"),
        "{img}"
    );
    assert!(img.contains("alt="), "{img}");
    assert!(
        with.html.contains("Bob -&gt; Alice"),
        "the source stays: {}",
        with.html
    );
    let puml = render("```puml\nA -> B\n```\n", &Fixed, "https://p.example");
    assert!(
        puml.html.contains("src=\"https://p.example/svg/"),
        "{}",
        puml.html
    );
    // A diagram too long for a URL is shown as source.
    let lines = long_diagram();
    let long = format!("```plantuml\n{lines}```\n");
    assert!(
        !render(&long, &Fixed, "https://p.example")
            .html
            .contains("<img")
    );
}

#[test]
fn maths_and_block_ids_are_shown_as_written() {
    let out = html("Euler: $e^{i\\pi} + 1 = 0$ ^block-1\n");
    assert!(out.contains("$e^{i\\pi} + 1 = 0$"), "{out}");
    assert!(out.contains("^block-1"), "{out}");
}

#[test]
fn hostile_fixtures_render_inert() {
    let fixtures = [
        "<script>alert(1)</script>",
        "<img src=x onerror=alert(1)>",
        "<iframe src=\"https://evil.example\"></iframe>",
        "<style>body{display:none}</style>",
        "<svg onload=alert(1)>",
        "[x](javascript:alert(1))",
        "[x](JaVaScRiPt:alert(1))",
        "[x](java\tscript:alert(1))",
        "[x](javascript&#58;alert(1))",
        "[x](&#106;avascript:alert(1))",
        "[x](vbscript:msgbox(1))",
        "[x](data:text/html,<script>alert(1)</script>)",
        "[x](https://ok.example \"title\\\" onmouseover=\\\"alert(1)\")",
        "![x](javascript:alert(1))",
        "<https://ok.example/\"onmouseover=\"alert(1)>",
        "[[Title<script>alert(1)</script>|label <img src=x onerror=1>]]",
        "> [!note] <img src=x onerror=alert(1)>\n> body",
        "> [!\"><script>]\n> body",
        "# <script>alert(1)</script>",
        "```\"><script>alert(1)</script>\ncode\n```",
        "| <script>x</script> | b |\n|---|---|\n| <img src=x onerror=1> | 2 |",
        "```mermaid\n</pre><script>alert(1)</script>\n```",
        "```mermaid\n%%{init: {\"securityLevel\": \"loose\"}}%%\nflowchart LR\nA[\"<img src=x onerror=alert(1)>\"]\n```",
        "==<script>x</script>==",
        "#<script>",
        "Text with <!-- comment --> and <? pi ?>",
        "[ref]: javascript:alert(1)\n\n[click][ref]",
        "<a href=\"javascript:alert(1)\">x</a>",
        "[x](<javascript:alert(1)>)",
        "Footnote[^<script>]\n\n[^<script>]: note",
    ];
    for fixture in fixtures {
        let out = render(fixture, &Fixed, "").html;
        assert_inert(&out);
        assert!(
            !out.to_ascii_lowercase().contains("<script"),
            "{fixture}: {out}"
        );
    }
    // With a PlantUML server, the only image is the diagram.
    let out = render(
        "```plantuml\nA -> B\n```\n<img src=x onerror=1>",
        &Fixed,
        "https://p.example",
    )
    .html;
    let images: Vec<_> = tags(&out)
        .into_iter()
        .filter(|t| t.starts_with("img "))
        .collect();
    assert_eq!(images.len(), 1, "{out}");
    assert!(images[0].contains("src=\"https://p.example/svg/"), "{out}");
}
