//! The Board showcase, for `just board-drive-showcase`
//! (`scripts/board-drive-showcase.sh`): four agents on two machines working
//! on three projects, for recording the README media. Developer tooling:
//! not published, not part of the server.
//!
//! `seed` writes the background documents of a fresh showcase store; `play`
//! runs one feature from idea to done on it, beat by beat, with a pause
//! after each (`DRIVE_PAUSE_MS`, default 3000; `0` waits for a keystroke).
//! Each agent is an API key named for its machine and agent, connecting as
//! that agent's MCP client. The script passes the MCP endpoint in
//! `SCRATCH_MCP_URL`, the tokens in `SHOWCASE_TOKEN_<KEY>` (for example
//! `SHOWCASE_TOKEN_MAC_CODEX`), and for `play` the folder of the agents'
//! logs in `SHOWCASE_LOG_DIR`. The content is the same on every run, so a
//! fresh store always gets the same document IDs.

#[path = "common/drive.rs"]
mod drive;

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use drive::{Client, Pause, Result};

/// Plans on the laptop: drafts the plan and closes it on sign-off.
const LAPTOP_CLAUDE: Agent = Agent {
    key: "laptop-claude",
    client: "claude-code",
};
/// The agent that writes the background of `project-b`.
const LAPTOP_OPENCODE: Agent = Agent {
    key: "laptop-opencode",
    client: "opencode",
};
/// On the Mac: shapes the idea, then builds the plan.
const MAC_CLAUDE: Agent = Agent {
    key: "mac-claude",
    client: "claude-code",
};
/// The agent that writes the background of `project-c`, and plan (3).
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
        Some("play") => Play::new(&url).await?.run().await,
        _ => Err("usage: board_drive_showcase seed|play".into()),
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

/// The idea (1) `mac-claude` drafts in `project-a`.
const IDEA_TITLE: &str = "Offline sync for field notes";
const IDEA_BODY: &str = "# Offline sync for field notes\n\n> [!abstract] Recommendation: `proceed-to-experiment`\n> Field teams lose notes written without a signal. Queue edits on the device and sync them when it is back online.\n\n## Problem\n\nA third of field visits have no signal for part of the day, and notes written then are lost when the app closes.\n\n## Proposed concept\n\n- Keep every edit in a local queue.\n- Push the queue when the device is online again.\n- Show which notes are waiting to sync.\n\n## Next experiment\n\nTen field workers use a build with the queue for a week; success is no lost note.\n";

/// The plan (2) `laptop-claude` writes from the idea.
const PLAN_TITLE: &str = "Delivery plan - Offline sync for field notes";
const PLAN_BODY: &str = "# Delivery plan - Offline sync for field notes\n\n> [!abstract] Plan status: `draft`\n> Queue edits on the device and sync them when it is back online (idea PA-1).\n\n## Steps\n\n| Step | Title |\n|---|---|\n| STEP-01 | Local queue for edits |\n| STEP-02 | Push the queue when back online |\n| STEP-03 | Pull changes made elsewhere |\n| STEP-04 | Waiting-to-sync badge on notes |\n| STEP-05 | Field test build |\n\n## Builder work log\n\n| Step | Status |\n|---|---|\n";

/// The plan (3) `mac-codex` drafts in `project-b`, which is not taken up.
const OTHER_PLAN_TITLE: &str = "Delivery plan - Rate limits for the sync API";
const OTHER_PLAN_BODY: &str = "# Delivery plan - Rate limits for the sync API\n\n> [!abstract] Plan status: `draft`\n> Limit each device to 60 sync requests a minute, with `429` and `Retry-After` above that.\n\n## Steps\n\n| Step | Title |\n|---|---|\n| STEP-01 | Token bucket per device |\n| STEP-02 | `429` replies with `Retry-After` |\n| STEP-03 | Dashboards and alerts |\n";

/// The titles of the five steps of plan (2), for its work log.
const STEPS: [&str; 5] = [
    "Local queue for edits",
    "Push the queue when back online",
    "Pull changes made elsewhere",
    "Waiting-to-sync badge on notes",
    "Field test build",
];

/// One run of the feature flow: the four agents, their logs and the pause.
struct Play {
    agents: Vec<(Agent, Client)>,
    logs: PathBuf,
    pause: Pause,
    started: Instant,
}

impl Play {
    /// Connects the four agents and empties their logs.
    async fn new(url: &str) -> Result<Self> {
        let logs = PathBuf::from(
            std::env::var("SHOWCASE_LOG_DIR").map_err(|_| "SHOWCASE_LOG_DIR is not set")?,
        );
        let mut agents = Vec::new();
        for agent in [LAPTOP_CLAUDE, LAPTOP_OPENCODE, MAC_CLAUDE, MAC_CODEX] {
            File::create(logs.join(format!("{}.log", agent.key)))?;
            agents.push((agent, agent.connect(url).await?));
        }
        Ok(Self {
            agents,
            logs,
            pause: Pause::from_env(),
            started: Instant::now(),
        })
    }

    fn client(&self, agent: Agent) -> Result<&Client> {
        self.agents
            .iter()
            .find(|(a, _)| a.key == agent.key)
            .map(|(_, c)| c)
            .ok_or_else(|| format!("{} is not connected", agent.key).into())
    }

    /// One beat by `agent`: a line on standard error, with the time since
    /// the start, and in the agent's log, with the time of day (UTC).
    fn beat(&self, agent: Agent, what: &str) -> Result<()> {
        writeln!(
            std::io::stderr(),
            "board-drive-showcase {:>6.1}s: [{}] {what}",
            self.started.elapsed().as_secs_f64(),
            agent.key
        )?;
        let day = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() % 86_400);
        let mut log = OpenOptions::new()
            .append(true)
            .open(self.logs.join(format!("{}.log", agent.key)))?;
        writeln!(
            log,
            "{:02}:{:02}:{:02} [{}] {what}",
            day / 3600,
            day / 60 % 60,
            day % 60,
            agent.key
        )?;
        Ok(())
    }

    async fn create(&self, agent: Agent, args: Value) -> Result<String> {
        drive::create(self.client(agent)?, args).await
    }

    async fn front(&self, agent: Agent, id: &str, set: Value, remove: &[&str]) -> Result<()> {
        drive::front(self.client(agent)?, id, set, remove).await
    }

    async fn append(&self, agent: Agent, id: &str, content: &str) -> Result<()> {
        drive::call(
            self.client(agent)?,
            "append_to_document",
            json!({"document_id": id, "content": content, "summary": "Showcase"}),
        )
        .await?;
        Ok(())
    }

    /// The beats B01 to B16, with three background beats by
    /// `laptop-opencode` after B04, B09 and B12.
    async fn run(self) -> Result<()> {
        let idea = self.idea().await?;
        let (plan, other) = self.plan(&idea).await?;
        self.other_plan(&other).await?;
        self.build(&plan).await?;
        self.close(&idea, &plan, &other).await?;
        for (_, client) in self.agents {
            client.cancel().await?;
        }
        Ok(())
    }

    /// B01 to B03: the idea is drafted, put to you, and accepted.
    async fn idea(&self) -> Result<String> {
        let idea = self
            .create(
                MAC_CLAUDE,
                json!({
                    "project": "project-a", "type": "ideas", "status": "draft",
                    "title": IDEA_TITLE, "content": IDEA_BODY, "summary": "Idea draft",
                }),
            )
            .await?;
        self.beat(MAC_CLAUDE, &format!("{idea} idea drafted: {IDEA_TITLE}"))?;
        self.pause
            .wait(&format!("{idea} goes to review, waiting for your decision"))
            .await?;
        self.front(
            MAC_CLAUDE,
            &idea,
            json!({"status": "review", "attention": "Waiting for your decision"}),
            &[],
        )
        .await?;
        self.beat(
            MAC_CLAUDE,
            &format!("{idea} draft -> review, waiting for your decision"),
        )?;
        self.pause.wait(&format!("you accept {idea}")).await?;
        self.front(
            MAC_CLAUDE,
            &idea,
            json!({"status": "approved"}),
            &["attention"],
        )
        .await?;
        self.beat(MAC_CLAUDE, &format!("{idea} accepted: review -> approved"))?;
        self.pause.wait("laptop-claude plans it").await?;
        Ok(idea)
    }

    /// B04 to B07, with background after B04: the plan is drafted from the
    /// idea, put to you, and approved, while `mac-codex` drafts plan (3).
    /// Returns the two plans' IDs.
    async fn plan(&self, idea: &str) -> Result<(String, String)> {
        let plan = self
            .create(
                LAPTOP_CLAUDE,
                json!({
                    "project": "project-a", "type": "plans", "status": "draft",
                    "title": PLAN_TITLE, "content": PLAN_BODY,
                    "front_matter": {"source_ideas": [idea], "implementation_status": "not-started"},
                    "summary": "Draft delivery plan",
                }),
            )
            .await?;
        self.beat(
            LAPTOP_CLAUDE,
            &format!("{plan} plan drafted from {idea}: {PLAN_TITLE}"),
        )?;
        self.pause
            .half("laptop-opencode updates the sync API spec")
            .await?;
        self.append(
            LAPTOP_OPENCODE,
            "PB-1",
            "\n## Cursor expiry\n\nA cursor stays valid for seven days; after that the device pulls everything again.\n",
        )
        .await?;
        self.beat(LAPTOP_OPENCODE, "PB-1 sync API spec: cursor expiry added")?;
        self.pause
            .half(&format!("{plan} goes to review, waiting for your approval"))
            .await?;
        self.front(
            LAPTOP_CLAUDE,
            &plan,
            json!({"status": "review", "attention": "Waiting for your approval"}),
            &[],
        )
        .await?;
        self.beat(
            LAPTOP_CLAUDE,
            &format!("{plan} draft -> review, waiting for your approval"),
        )?;
        self.pause
            .wait("mac-codex drafts a plan in project-b")
            .await?;
        let other = self
            .create(
                MAC_CODEX,
                json!({
                    "project": "project-b", "type": "plans", "status": "draft",
                    "title": OTHER_PLAN_TITLE, "content": OTHER_PLAN_BODY,
                    "front_matter": {"implementation_status": "not-started"},
                    "summary": "Draft delivery plan",
                }),
            )
            .await?;
        self.beat(
            MAC_CODEX,
            &format!("{other} plan drafted: {OTHER_PLAN_TITLE}"),
        )?;
        self.pause.wait(&format!("you approve {plan}")).await?;
        self.front(
            LAPTOP_CLAUDE,
            &plan,
            json!({"status": "approved", "build_ready": true}),
            &["attention"],
        )
        .await?;
        self.beat(
            LAPTOP_CLAUDE,
            &format!("{plan} approved: review -> approved"),
        )?;
        self.pause.wait(&format!("{other} goes to review")).await?;
        Ok((plan, other))
    }

    /// B08: plan (3) is put to you, and stays lit while plan (2) is built.
    async fn other_plan(&self, other: &str) -> Result<()> {
        self.front(
            MAC_CODEX,
            other,
            json!({"status": "review", "attention": "Waiting for your approval"}),
            &[],
        )
        .await?;
        self.beat(
            MAC_CODEX,
            &format!("{other} draft -> review, waiting for your approval"),
        )?;
        self.pause.wait("mac-claude starts building").await?;
        Ok(())
    }

    /// B09 to B14, with background after B09 and B12: the plan is built
    /// from STEP-01 to STEP-05 and waits for your sign-off.
    async fn build(&self, plan: &str) -> Result<()> {
        self.front(
            MAC_CLAUDE,
            plan,
            json!({
                "implementation_status": "in-progress", "current_step": "STEP-01",
                "execution_branch": "feature/pa-2-offline-sync-for-field-notes",
            }),
            &[],
        )
        .await?;
        self.beat(
            MAC_CLAUDE,
            &format!("{plan} build started: STEP-01 {}", STEPS[0]),
        )?;
        self.pause
            .half("laptop-opencode picks up the retry issue")
            .await?;
        self.front(LAPTOP_OPENCODE, "PB-2", json!({"status": "review"}), &[])
            .await?;
        self.beat(
            LAPTOP_OPENCODE,
            "PB-2 retry issue: draft -> review (investigating)",
        )?;
        for (n, title) in STEPS.iter().enumerate().skip(1) {
            self.pause.half(&format!("{plan} STEP-0{}", n + 1)).await?;
            self.append(MAC_CLAUDE, plan, &format!("| STEP-0{n} | completed |\n"))
                .await?;
            self.front(
                MAC_CLAUDE,
                plan,
                json!({"current_step": format!("STEP-0{}", n + 1)}),
                &[],
            )
            .await?;
            self.beat(
                MAC_CLAUDE,
                &format!("{plan} STEP-0{n} done, now STEP-0{}: {title}", n + 1),
            )?;
            if n + 1 == 4 {
                self.pause
                    .half("laptop-opencode updates the release checklist")
                    .await?;
                self.append(LAPTOP_OPENCODE, "PB-3", "- [ ] Rollback rehearsed\n")
                    .await?;
                self.beat(
                    LAPTOP_OPENCODE,
                    "PB-3 release checklist: rollback step added",
                )?;
            }
        }
        self.pause.wait(&format!("{plan} build completes")).await?;
        self.append(MAC_CLAUDE, plan, "| STEP-05 | completed |\n")
            .await?;
        self.front(
            MAC_CLAUDE,
            plan,
            json!({"implementation_status": "completed", "attention": "Build complete: waiting for your sign-off"}),
            &["current_step"],
        )
        .await?;
        self.beat(
            MAC_CLAUDE,
            &format!("{plan} build complete, waiting for your sign-off"),
        )?;
        self.pause.wait("you sign it off").await?;
        Ok(())
    }

    /// B15 and B16: plan (2) and the idea are done; plan (3) is rejected.
    async fn close(&self, idea: &str, plan: &str, other: &str) -> Result<()> {
        self.front(
            LAPTOP_CLAUDE,
            plan,
            json!({"status": "done", "signed_off_by": "Joe Lee"}),
            &["attention"],
        )
        .await?;
        self.front(LAPTOP_CLAUDE, idea, json!({"status": "done"}), &[])
            .await?;
        self.beat(
            LAPTOP_CLAUDE,
            &format!("{plan} signed off: approved -> done, and {idea} done"),
        )?;
        self.pause.wait(&format!("you reject {other}")).await?;
        self.front(
            MAC_CODEX,
            other,
            json!({"status": "rejected"}),
            &["attention"],
        )
        .await?;
        self.beat(MAC_CODEX, &format!("{other} rejected: review -> rejected"))?;
        Ok(())
    }
}
