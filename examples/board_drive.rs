//! Drives an open Board on a scratch server with a scripted run of MCP
//! writes, for `just board-drive` (`scripts/board-drive.sh`). Developer
//! tooling and the Board's demo script: not published, not part of the
//! server.
//!
//! Reads the MCP endpoint from `SCRATCH_MCP_URL`, an API key with write on
//! the `demo` and `sandbox` projects from `SCRATCH_TOKEN`, and the pause
//! between steps from `DRIVE_PAUSE_MS` (default 3000). Writes one line per
//! step on standard error, and on standard output the ID of the document it
//! archived last, for the script to restore with a local `joedocs` command
//! (a write from another process).

use std::error::Error;
use std::io::Write as _;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rmcp::model::{CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation};
use rmcp::service::RunningService;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::{RoleClient, ServiceExt as _};
use serde_json::{Value, json};

type Client = RunningService<RoleClient, ClientConfig>;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

/// How many documents the burst moves, about 200 ms apart.
const BURST: usize = 10;

/// A run against one server.
struct Drive {
    client: Client,
    pause: Duration,
    started: Instant,
    /// Makes this run's titles unique, so runs can repeat on one store.
    run: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let url = std::env::var("SCRATCH_MCP_URL").map_err(|_| "SCRATCH_MCP_URL is not set")?;
    let token = std::env::var("SCRATCH_TOKEN").map_err(|_| "SCRATCH_TOKEN is not set")?;
    let pause = std::env::var("DRIVE_PAUSE_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000);
    // reqwest is built with rustls but no crypto provider, as in the tests.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(url).auth_header(token),
    );
    let client = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("board-drive", "1.0"),
    )
    .serve(transport)
    .await?;
    let drive = Drive {
        client,
        pause: Duration::from_millis(pause),
        started: Instant::now(),
        run: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() % 100_000),
    };
    let plan = drive.plan_through_approval().await?;
    drive.build(&plan).await?;
    let moved = drive.burst("draft", "review").await?;
    let issue = drive.lit_issue().await?;
    drive.move_back(&moved).await?;
    drive.close(&plan, &issue).await?;
    drive.client.cancel().await?;
    writeln!(std::io::stdout(), "{issue}")?;
    Ok(())
}

impl Drive {
    /// One line of progress on standard error, with the time since start.
    fn say(&self, message: &str) -> Result<()> {
        writeln!(
            std::io::stderr(),
            "board-drive {:>6.1}s: {message}",
            self.started.elapsed().as_secs_f64()
        )?;
        Ok(())
    }

    async fn wait(&self) {
        tokio::time::sleep(self.pause).await;
    }

    /// Calls `tool`, failing on a tool error, and returns its result.
    async fn call(&self, tool: &str, args: Value) -> Result<Value> {
        let args = args.as_object().cloned().unwrap_or_default();
        let result = self
            .client
            .call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(args))
            .await?;
        let content = result.structured_content.unwrap_or(Value::Null);
        if result.is_error == Some(true) {
            return Err(format!("{tool} failed: {content}").into());
        }
        Ok(content)
    }

    /// Sets front matter keys on `id`, and removes `remove`.
    async fn front(&self, id: &str, set: Value, remove: &[&str]) -> Result<()> {
        let mut args = json!({"document_id": id, "set": set});
        if !remove.is_empty() {
            args["remove"] = json!(remove);
        }
        self.call("change_document_front_matter", args).await?;
        Ok(())
    }

    async fn create(&self, args: Value) -> Result<String> {
        let made = self.call("new_document", args).await?;
        Ok(made["document_id"]
            .as_str()
            .ok_or("new_document returned no document_id")?
            .to_owned())
    }

    /// A plan is drafted, lit for approval, reviewed and approved.
    async fn plan_through_approval(&self) -> Result<String> {
        let plan = self
            .create(json!({
                "project": "demo", "type": "plans", "status": "draft",
                "title": format!("Board demo plan {}", self.run),
                "content": "# Board demo plan\n\nStep one.\n",
                "summary": "Board drive",
            }))
            .await?;
        self.say(&format!("created {plan} (plans, draft)"))?;
        self.wait().await;
        self.front(
            &plan,
            json!({"attention": "Waiting for your approval"}),
            &[],
        )
        .await?;
        self.say(&format!("{plan}: attention set"))?;
        self.wait().await;
        self.front(&plan, json!({"status": "review"}), &[]).await?;
        self.say(&format!("{plan}: draft -> review"))?;
        self.wait().await;
        self.front(&plan, json!({"status": "approved"}), &["attention"])
            .await?;
        self.say(&format!("{plan}: review -> approved, attention removed"))?;
        self.wait().await;
        Ok(plan)
    }

    /// A builder picks the plan up and edits it twice.
    async fn build(&self, plan: &str) -> Result<()> {
        self.front(plan, json!({"implementation_status": "in-progress"}), &[])
            .await?;
        self.say(&format!("{plan}: the builder picked it up (in-progress)"))?;
        self.wait().await;
        for n in 2..=3 {
            self.call(
                "append_to_document",
                json!({"document_id": plan, "content": format!("\nStep {n}.\n"), "summary": "Board drive"}),
            )
            .await?;
            self.say(&format!("{plan}: edited (a new revision)"))?;
            tokio::time::sleep(self.pause / 2).await;
        }
        self.wait().await;
        Ok(())
    }

    /// Up to [`BURST`] sample documents move from `from` to `to`, about
    /// 200 ms apart. Returns their IDs.
    async fn burst(&self, from: &str, to: &str) -> Result<Vec<String>> {
        let found = self
            .call(
                "search_documents",
                json!({"project": "demo", "filters": {"status": from, "tags": {"contains": "sample"}}, "limit": BURST}),
            )
            .await?;
        let ids: Vec<String> = found["items"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|i| i["id"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        self.say(&format!("burst: {} documents {from} -> {to}", ids.len()))?;
        for id in &ids {
            self.front(id, json!({"status": to}), &[]).await?;
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        self.wait().await;
        self.wait().await;
        Ok(ids)
    }

    /// An issue arrives in the other project, lit.
    async fn lit_issue(&self) -> Result<String> {
        let issue = self
            .create(json!({
                "project": "sandbox", "type": "issues", "status": "draft",
                "title": format!("Board demo issue {}", self.run),
                "content": "# Board demo issue\n",
                "front_matter": {"attention": "Critical: blocks the release"},
                "summary": "Board drive",
            }))
            .await?;
        self.say(&format!("created {issue} in sandbox, with attention"))?;
        self.wait().await;
        Ok(issue)
    }

    /// The burst's documents go back.
    async fn move_back(&self, ids: &[String]) -> Result<()> {
        self.say(&format!(
            "burst back: {} documents review -> draft",
            ids.len()
        ))?;
        for id in ids {
            self.front(id, json!({"status": "draft"}), &[]).await?;
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        self.wait().await;
        self.wait().await;
        Ok(())
    }

    /// The build completes and the plan is closed; the issue is archived.
    async fn close(&self, plan: &str, issue: &str) -> Result<()> {
        self.front(plan, json!({"implementation_status": "completed"}), &[])
            .await?;
        self.say(&format!("{plan}: build completed, awaiting sign-off"))?;
        self.wait().await;
        self.front(plan, json!({"status": "done"}), &[]).await?;
        self.say(&format!("{plan}: approved -> done (closed)"))?;
        self.wait().await;
        self.call(
            "archive_document",
            json!({"document_id": issue, "reason": "Board drive"}),
        )
        .await?;
        self.say(&format!(
            "{issue}: archived; the script restores it with a local command"
        ))?;
        Ok(())
    }
}
