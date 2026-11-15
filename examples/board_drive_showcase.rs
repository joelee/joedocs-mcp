//! The Board showcase, for `just board-drive-showcase`
//! (`scripts/board-drive-showcase.sh`): four agents on two machines working
//! on three projects, for recording the README media. Developer tooling:
//! not published, not part of the server.
//!
//! `seed` writes the background documents of a fresh showcase store. Each
//! agent is an API key named for its machine and agent, connecting as that
//! agent's MCP client. The script passes the MCP endpoint in
//! `SCRATCH_MCP_URL` and the tokens in `SHOWCASE_TOKEN_<KEY>` (for example
//! `SHOWCASE_TOKEN_MAC_CODEX`); the content is the same on every run, so a
//! fresh store always gets the same document IDs.

#[expect(
    dead_code,
    reason = "the pause between beats is for play, which comes next"
)]
#[path = "common/drive.rs"]
mod drive;

use std::io::Write as _;

use serde_json::json;

use drive::{Client, Result};

/// The agent that writes the background of `project-b`.
const LAPTOP_OPENCODE: Agent = Agent {
    key: "laptop-opencode",
    client: "opencode",
};
/// The agent that writes the background of `project-c`.
const MAC_CODEX: Agent = Agent {
    key: "mac-codex",
    client: "codex",
};

/// An API key and the MCP client name it connects with.
#[derive(Clone, Copy)]
struct Agent {
    key: &'static str,
    client: &'static str,
}

impl Agent {
    /// Connects with this agent's token from `SHOWCASE_TOKEN_<KEY>`.
    async fn connect(self, url: &str) -> Result<Client> {
        let var = format!(
            "SHOWCASE_TOKEN_{}",
            self.key.to_ascii_uppercase().replace('-', "_")
        );
        let token = std::env::var(&var).map_err(|_| format!("{var} is not set"))?;
        drive::connect(url, &token, self.client).await
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let url = std::env::var("SCRATCH_MCP_URL").map_err(|_| "SCRATCH_MCP_URL is not set")?;
    match std::env::args().nth(1).as_deref() {
        Some("seed") => seed(&url).await,
        _ => Err("usage: board_drive_showcase seed".into()),
    }
}

/// One line of progress on standard error.
fn say(message: &str) -> Result<()> {
    writeln!(std::io::stderr(), "board-drive-showcase: {message}")?;
    Ok(())
}

/// A background document: project, type, status, title and body.
type Background = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
);

/// The background of `project-b` (PB-1 to PB-3), by `laptop-opencode`.
const PROJECT_B: [Background; 3] = [
    (
        "project-b",
        "specs",
        "review",
        "Sync API specification",
        "# Sync API specification\n\nThe endpoints the field notes app uses to push and pull changes.\n\n## Endpoints\n\n| Method | Path | Purpose |\n|---|---|---|\n| `POST` | `/sync/push` | Send local changes since the last sync |\n| `GET` | `/sync/pull?since=<cursor>` | Fetch changes made elsewhere |\n\n## Open questions\n\n- How long a cursor stays valid.\n",
    ),
    (
        "project-b",
        "issues",
        "draft",
        "Issue - Sync retries flood the log",
        "# Issue - Sync retries flood the log\n\n> [!bug] `bug`, severity `low`, status `draft`\n> A device offline for a day writes one warning per retry.\n\n## Steps to reproduce\n\n1. Turn off the network on a device.\n2. Edit a note and wait an hour.\n\n## Actual behaviour\n\nAbout 700 identical warnings.\n",
    ),
    (
        "project-b",
        "notes",
        "approved",
        "Release checklist for the sync service",
        "# Release checklist for the sync service\n\n- [ ] Database backup taken\n- [ ] Canary on one region for an hour\n- [ ] Error rate under 0.1%\n- [ ] Changelog published\n",
    ),
];

/// The background of `project-c` (PC-1 to PC-3), by `mac-codex`.
const PROJECT_C: [Background; 3] = [
    (
        "project-c",
        "research",
        "draft",
        "Research - Conflict resolution strategies",
        "# Research - Conflict resolution strategies\n\nLast writer wins, three-way merge, or CRDTs for notes edited on two devices.\n\n| Strategy | Data loss | Effort |\n|---|---|---|\n| Last writer wins | Possible | Low |\n| Three-way merge | Rare | Medium |\n| CRDT | None | High |\n",
    ),
    (
        "project-c",
        "reviews",
        "review",
        "Review - Field notes editor PR 41",
        "# Review - Field notes editor PR 41\n\n> [!abstract] Verdict: `approve-with-comments`\n> One Medium finding: the autosave timer is not cancelled when the editor closes.\n",
    ),
    (
        "project-c",
        "ideas",
        "parked",
        "Shared field notes between teams",
        "# Shared field notes between teams\n\n> [!abstract] Recommendation: `park`\n> Worth doing after offline sync ships; sharing needs the same conflict handling.\n",
    ),
];

/// Writes the background documents of a fresh store.
async fn seed(url: &str) -> Result<()> {
    for (agent, documents) in [(LAPTOP_OPENCODE, &PROJECT_B), (MAC_CODEX, &PROJECT_C)] {
        let client = agent.connect(url).await?;
        for (project, doc_type, status, title, content) in documents {
            let id = drive::create(
                &client,
                json!({
                    "project": project, "type": doc_type, "status": status,
                    "title": title, "content": content,
                    "summary": "Showcase background",
                }),
            )
            .await?;
            say(&format!(
                "[{}] {id} {doc_type} ({status}): {title}",
                agent.key
            ))?;
        }
        client.cancel().await?;
    }
    Ok(())
}
