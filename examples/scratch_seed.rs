//! Fills a scratch store with sample documents over MCP, for
//! `just scratch-server` (`scripts/scratch-server.sh` runs it once, when the
//! store is new). Developer tooling: not published, not part of the server.
//!
//! Reads the MCP endpoint from `SCRATCH_MCP_URL` and an API key with write on
//! the `demo` and `sandbox` projects from `SCRATCH_TOKEN`. The content is the
//! same on every run: no clock or randomness.
//!
//! `demo` gets more than one page of documents (the web interface and
//! `list` show 200), in every default type and status, plus documents with
//! diagrams, callouts and links, several revisions, a multi-file backlog and
//! an archived one; `sandbox` gets a few.

use std::error::Error;
use std::io::Write as _;

use rmcp::model::{CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation};
use rmcp::service::RunningService;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::{RoleClient, ServiceExt as _};
use serde_json::{Value, json};

type Client = RunningService<RoleClient, ClientConfig>;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const TYPES: [&str; 8] = [
    "home", "plans", "ideas", "issues", "reviews", "research", "specs", "notes",
];
const STATUSES: [&str; 7] = [
    "draft",
    "review",
    "approved",
    "rejected",
    "superseded",
    "done",
    "parked",
];
/// Documents made in bulk in `demo`: more than one page of 200.
const BULK: usize = 210;

#[tokio::main]
async fn main() -> Result<()> {
    let url = std::env::var("SCRATCH_MCP_URL").map_err(|_| "SCRATCH_MCP_URL is not set")?;
    let token = std::env::var("SCRATCH_TOKEN").map_err(|_| "SCRATCH_TOKEN is not set")?;
    // reqwest is built with rustls but no crypto provider, as in the tests.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(url).auth_header(token),
    );
    let client = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("scratch-seed", "1.0"),
    )
    .serve(transport)
    .await?;

    bulk(&client).await?;
    showcase(&client).await?;
    histories(&client).await?;
    backlog(&client).await?;
    archived(&client).await?;
    sandbox(&client).await?;

    client.cancel().await?;
    say("sample data written")?;
    Ok(())
}

/// One line of progress on standard error.
fn say(message: &str) -> Result<()> {
    writeln!(std::io::stderr(), "scratch-seed: {message}")?;
    Ok(())
}

/// Calls `tool`, failing on a tool error, and returns its structured result.
async fn call(client: &Client, tool: &str, args: Value) -> Result<Value> {
    let args = args.as_object().cloned().unwrap_or_default();
    let result = client
        .call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(args))
        .await?;
    let content = result.structured_content.unwrap_or(Value::Null);
    if result.is_error == Some(true) {
        return Err(format!("{tool} failed: {content}").into());
    }
    Ok(content)
}

/// Creates a document and returns its ID and revision.
async fn create(
    client: &Client,
    project: &str,
    (doc_type, status): (&str, &str),
    title: &str,
    content: &str,
    front_matter: Value,
) -> Result<(String, u64)> {
    let made = call(
        client,
        "new_document",
        json!({
            "project": project,
            "type": doc_type,
            "status": status,
            "title": title,
            "content": content,
            "front_matter": front_matter,
            "summary": "Sample data",
        }),
    )
    .await?;
    let id = made["document_id"]
        .as_str()
        .ok_or("new_document returned no document_id")?
        .to_owned();
    let revision = made["revision"].as_u64().unwrap_or(1);
    Ok((id, revision))
}

/// Replaces `old` with `new` in a document and returns the new revision.
async fn edit(client: &Client, id: &str, revision: u64, old: &str, new: &str) -> Result<u64> {
    let changed = call(
        client,
        "edit_document",
        json!({
            "document_id": id,
            "expected_revision": revision,
            "edits": [{"old": old, "new": new}],
            "summary": "Sample revision",
        }),
    )
    .await?;
    changed["revision"]
        .as_u64()
        .ok_or_else(|| "edit_document returned no revision".into())
}

/// `BULK` short documents in `demo`, cycling through every type and status.
async fn bulk(client: &Client) -> Result<()> {
    for i in 0..BULK {
        let doc_type = TYPES[i % TYPES.len()];
        let status = STATUSES[(i / TYPES.len()) % STATUSES.len()];
        let title = format!("Sample {doc_type} {:03}", i + 1);
        let content = format!(
            "# {title}\n\nA sample {doc_type} document, status `{status}`, for trying the \
             web interface: paging, sorting and filters.\n\n- Item one\n- Item two\n"
        );
        let tags = json!({"tags": ["sample", doc_type]});
        create(client, "demo", (doc_type, status), &title, &content, tags).await?;
        if (i + 1) % 50 == 0 {
            say(&format!("{} sample documents", i + 1))?;
        }
    }
    Ok(())
}

/// Documents that show the renderer: diagrams, callouts, tables, tasks and
/// links between documents.
async fn showcase(client: &Client) -> Result<()> {
    let overview = "# Architecture overview\n\n\
        > [!note] About this document\n\
        > Sample content for the scratch server. See [[Release checklist]] and \
        [[Product backlog]].\n\n\
        ## Components\n\n\
        ```mermaid\n\
        flowchart LR\n\
        A[\"Agents\"] -->|MCP| S[\"JoeDocs server\"]\n\
        S --> V[\"Vault\"]\n\
        S --> H[\"History\"]\n\
        S --> D[\"Index\"]\n\
        ```\n\n\
        ## Sign-in\n\n\
        ```plantuml\n\
        @startuml\n\
        actor User\n\
        User -> Server : password and code\n\
        Server --> User : session\n\
        @enduml\n\
        ```\n\n\
        ## Decisions\n\n\
        | Decision | Why |\n\
        |---|---|\n\
        | Markdown files | Readable in Obsidian |\n\
        | Git history | Every revision kept |\n\n\
        > [!warning] Sample warning\n\
        > Callouts render with their own colour.\n\n\
        A ==highlight== and a #sample tag.\n";
    create(
        client,
        "demo",
        ("specs", "approved"),
        "Architecture overview",
        overview,
        json!({"tags": ["architecture"], "owner": "dev"}),
    )
    .await?;
    say("showcase document")?;
    Ok(())
}

/// Three documents with several revisions each, for History and diffs.
async fn histories(client: &Client) -> Result<()> {
    let checklist = "# Release checklist\n\n- [ ] Changelog\n- [ ] Version\n- [ ] Tag\n";
    let (id, rev) = create(
        client,
        "demo",
        ("notes", "review"),
        "Release checklist",
        checklist,
        json!({}),
    )
    .await?;
    let rev = edit(client, &id, rev, "- [ ] Changelog", "- [x] Changelog").await?;
    let rev = edit(client, &id, rev, "- [ ] Version", "- [x] Version").await?;
    edit(client, &id, rev, "- [ ] Tag", "- [ ] Tag\n- [ ] Announce").await?;

    let tuning = "# Search tuning notes\n\nRanking: title first.\n\nOpen question: stemming.\n";
    let (id, rev) = create(
        client,
        "demo",
        ("research", "draft"),
        "Search tuning notes",
        tuning,
        json!({}),
    )
    .await?;
    let rev = edit(
        client,
        &id,
        rev,
        "Ranking: title first.",
        "Ranking: title first, then body.",
    )
    .await?;
    let rev = edit(
        client,
        &id,
        rev,
        "Open question: stemming.",
        "Stemming: on, with English rules.",
    )
    .await?;
    edit(
        client,
        &id,
        rev,
        "with English rules.",
        "with English rules.\n\nNext: phrase queries.",
    )
    .await?;

    let onboarding = "# Onboarding guide\n\n1. Install\n2. Sign in\n";
    let (id, rev) = create(
        client,
        "demo",
        ("home", "approved"),
        "Onboarding guide",
        onboarding,
        json!({}),
    )
    .await?;
    let rev = edit(client, &id, rev, "2. Sign in", "2. Sign in\n3. Add a key").await?;
    let rev = edit(client, &id, rev, "1. Install", "1. Install the binary").await?;
    edit(
        client,
        &id,
        rev,
        "3. Add a key",
        "3. Add a key\n4. Connect an agent",
    )
    .await?;
    say("documents with history")?;
    Ok(())
}

/// A multi-file document: a backlog with one file per item.
async fn backlog(client: &Client) -> Result<()> {
    let made = call(
        client,
        "new_document",
        json!({
            "project": "demo",
            "type": "notes",
            "status": "draft",
            "title": "Product backlog",
            "content": "# Product backlog\n\nOne file per item, each with its own `state`.\n",
            "multifile": true,
            "summary": "Sample data",
        }),
    )
    .await?;
    let id = made["document_id"]
        .as_str()
        .ok_or("new_document returned no document_id")?
        .to_owned();
    let items = [
        ("items/dark-mode", "Dark mode", "planned"),
        ("items/export-pdf", "Export to PDF", "future"),
        ("items/bulk-edit", "Bulk edit", "in-progress"),
    ];
    for (file, title, state) in items {
        call(
            client,
            "new_document_file",
            json!({
                "document_id": id,
                "file": file,
                "content": format!("# {title}\n\nA sample backlog item.\n"),
                "front_matter": {"state": state, "kind": "feature"},
                "summary": "Sample data",
            }),
        )
        .await?;
    }
    say("multi-file backlog")?;
    Ok(())
}

/// A document that is then archived, for the Archive page.
async fn archived(client: &Client) -> Result<()> {
    let (id, _) = create(
        client,
        "demo",
        ("notes", "done"),
        "Old meeting notes",
        "# Old meeting notes\n\nKept only in the archive.\n",
        json!({}),
    )
    .await?;
    call(
        client,
        "archive_document",
        json!({"document_id": id, "reason": "Sample archived document"}),
    )
    .await?;
    say("archived document")?;
    Ok(())
}

/// A few documents in the second project.
async fn sandbox(client: &Client) -> Result<()> {
    let docs = [
        ("home", "approved", "Sandbox home"),
        ("ideas", "draft", "Try a new layout"),
        ("issues", "review", "Login button misaligned"),
        ("plans", "parked", "Later experiment"),
    ];
    for (doc_type, status, title) in docs {
        let content = format!("# {title}\n\nSample document in the sandbox project.\n");
        create(
            client,
            "sandbox",
            (doc_type, status),
            title,
            &content,
            json!({}),
        )
        .await?;
    }
    say("sandbox documents")?;
    Ok(())
}
