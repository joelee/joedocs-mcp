//! Black-box tests against a running `joedocs serve`, such as the Docker
//! Compose stack. They are ignored by default; `just docker-test` builds a
//! throwaway stack, runs them, restarts it, runs them again, and removes it.
//!
//! To run them against another server, create a project and a key with
//! write access to it, then:
//!
//! ```text
//! JOEDOCS_TEST_URL=http://127.0.0.1:8750 JOEDOCS_TEST_TOKEN=jd_... \
//! JOEDOCS_TEST_PROJECT=itest cargo nextest run --test deployment --run-ignored only
//! ```
//!
//! `JOEDOCS_TEST_PHASE=after-restart` switches the restart test from
//! writing its document to checking it survived.

mod common;

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use common::{Client, INIT, TestResult, connect, error, ok, post, status};

const IGNORED: &str = "needs a running server; run `just docker-test`";

/// The server under test, from the environment.
struct Target {
    /// `http://host:port`, without a trailing slash.
    base: String,
    /// `host:port`.
    authority: String,
    token: String,
    project: String,
}

fn target() -> TestResult<Target> {
    let base = std::env::var("JOEDOCS_TEST_URL").map_err(
        |_| "set JOEDOCS_TEST_URL, e.g. http://127.0.0.1:8750 (or run `just docker-test`)",
    )?;
    let base = base.trim_end_matches('/').to_owned();
    let authority = base
        .strip_prefix("http://")
        .ok_or("JOEDOCS_TEST_URL must start with http://")?
        .to_owned();
    Ok(Target {
        base,
        authority,
        token: std::env::var("JOEDOCS_TEST_TOKEN").map_err(|_| "set JOEDOCS_TEST_TOKEN")?,
        project: std::env::var("JOEDOCS_TEST_PROJECT").unwrap_or_else(|_| "itest".to_owned()),
    })
}

impl Target {
    async fn client(&self) -> TestResult<Client> {
        connect(&format!("{}/mcp", self.base), &self.token).await
    }

    fn auth(&self) -> String {
        format!("Authorization: Bearer {}\r\n", self.token)
    }
}

/// A title no earlier run has used, so the suite can rerun on one server.
fn unique(prefix: &str) -> TestResult<String> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(format!("{prefix} {nanos}"))
}

#[tokio::test]
#[ignore = "needs a running server; run `just docker-test`"]
async fn health_needs_no_key() {
    let t = target().unwrap();
    let request = format!(
        "GET /health HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        t.authority
    );
    assert_eq!(
        status(&t.authority, &request).await.unwrap(),
        200,
        "{IGNORED}"
    );
}

#[tokio::test]
#[ignore = "needs a running server; run `just docker-test`"]
async fn requests_without_a_valid_key_are_refused() {
    let t = target().unwrap();
    let host = &t.authority;
    let wrong = format!(
        "Authorization: Bearer jd_{}_{}\r\n",
        "0".repeat(12),
        "1".repeat(64)
    );
    for extra in ["", wrong.as_str(), "Authorization: Basic eDp5\r\n"] {
        assert_eq!(
            status(host, &post(host, extra, INIT)).await.unwrap(),
            401,
            "{extra}"
        );
    }
    assert_eq!(
        status(host, &post(host, &t.auth(), INIT)).await.unwrap(),
        200
    );
}

#[tokio::test]
#[ignore = "needs a running server; run `just docker-test`"]
async fn foreign_hosts_and_origins_are_refused() {
    let t = target().unwrap();
    let host = &t.authority;
    let evil_origin = format!("{}Origin: http://evil.example\r\n", t.auth());
    assert_eq!(
        status(host, &post(host, &evil_origin, INIT)).await.unwrap(),
        403
    );
    assert_eq!(
        status(host, &post("evil.example", &t.auth(), INIT))
            .await
            .unwrap(),
        403
    );
}

#[tokio::test]
#[ignore = "needs a running server; run `just docker-test`"]
async fn the_key_can_write_its_project() {
    let t = target().unwrap();
    let client = t.client().await.unwrap();
    let tools = client.list_all_tools().await.unwrap();
    assert_eq!(tools.len(), 15);
    let projects = ok(&client, "list_projects", json!({})).await.unwrap();
    let project = projects["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == t.project.as_str())
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(project["permission"], "write", "{projects}");
    client.cancel().await.unwrap();
}

/// Edits the lifecycle document, checks the diff, and finds it by search.
async fn edit_diff_and_search(
    client: &Client,
    project: &str,
    id: &str,
    title: &str,
) -> TestResult<()> {
    let edited = ok(
        client,
        "edit_document",
        json!({"document_id": id, "edits": [{"old": "# Notes v2", "new": "# Notes v3"}]}),
    )
    .await?;
    assert_eq!(edited["revision"], 5);
    let diff = ok(
        client,
        "diff_document",
        json!({"document_id": id, "from": 4, "context": 0}),
    )
    .await?;
    let text = diff["diff"].as_str().ok_or("no diff")?;
    assert!(text.contains("-# Notes v2\n+# Notes v3\n"), "{diff}");

    let found = ok(
        client,
        "search_documents",
        json!({"project": project, "query": format!("\"{title}\""), "filters": {"owner": "itest"}}),
    )
    .await?;
    assert_eq!(found["items"][0]["id"], id, "{found}");
    Ok(())
}

#[tokio::test]
#[ignore = "needs a running server; run `just docker-test`"]
async fn a_document_lifecycle_works() {
    let t = target().unwrap();
    let client = t.client().await.unwrap();
    let title = unique("Lifecycle").unwrap();

    let created = ok(
        &client,
        "new_document",
        json!({"project": t.project, "type": "notes", "title": title,
               "content": "# Notes\n\n```mermaid\nflowchart LR\n  a --> b\n```\n",
               "front_matter": {"tags": ["itest"]}}),
    )
    .await
    .unwrap();
    let id = created["document_id"].as_str().unwrap().to_owned();
    assert_eq!(created["revision"], 1);
    assert_eq!(created["front_matter"]["client"], "test-client");

    let got = ok(&client, "get_document", json!({"document_id": id}))
        .await
        .unwrap();
    assert!(got["markdown"].as_str().unwrap().contains("```mermaid\n"));

    let stale = error(
        &client,
        "update_document",
        json!({"document_id": id, "content": "x", "expected_revision": 99}),
    )
    .await
    .unwrap();
    assert_eq!(
        (stale["code"].clone(), stale["current_revision"].clone()),
        (json!("conflict"), json!(1))
    );

    ok(
        &client,
        "update_document",
        json!({"document_id": id, "content": "# Notes v2\n", "expected_revision": 1}),
    )
    .await
    .unwrap();
    let renamed = ok(
        &client,
        "rename_document",
        json!({"document_id": id, "title": format!("{title} renamed")}),
    )
    .await
    .unwrap();
    assert_eq!(renamed["front_matter"]["aliases"], json!([title]));
    let changed = ok(
        &client,
        "change_document_front_matter",
        json!({"document_id": id, "set": {"status": "done", "owner": "itest"}}),
    )
    .await
    .unwrap();
    assert_eq!(changed["revision"], 4);

    let listed = ok(
        &client,
        "list_documents",
        json!({"project": t.project, "type": "notes", "status": "done", "sort": "created_desc", "limit": 1000}),
    )
    .await
    .unwrap();
    assert!(
        listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["id"] == id.as_str()),
        "{listed}"
    );

    edit_diff_and_search(&client, &t.project, &id, &title)
        .await
        .unwrap();

    let first = ok(
        &client,
        "get_document",
        json!({"document_id": id, "version": 1}),
    )
    .await
    .unwrap();
    assert!(first["markdown"].as_str().unwrap().contains("# Notes\n"));

    ok(
        &client,
        "archive_document",
        json!({"document_id": id, "reason": "deployment test"}),
    )
    .await
    .unwrap();
    let gone = error(&client, "get_document", json!({"document_id": id}))
        .await
        .unwrap();
    assert_eq!(gone["code"], "archived");
    client.cancel().await.unwrap();
}

/// Before a restart this writes "Restart check" at revision 2; after one
/// (`JOEDOCS_TEST_PHASE=after-restart`) it checks the document, its index
/// entry and its history are all still there.
#[tokio::test]
#[ignore = "needs a running server; run `just docker-test`"]
async fn documents_survive_a_restart() {
    const TITLE: &str = "Restart check";
    let t = target().unwrap();
    let client = t.client().await.unwrap();
    let after = std::env::var("JOEDOCS_TEST_PHASE").is_ok_and(|phase| phase == "after-restart");
    let listed = ok(
        &client,
        "list_documents",
        json!({"project": t.project, "sort": "title", "limit": 1000}),
    )
    .await
    .unwrap();
    let existing = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["title"] == TITLE)
        .map(|d| d["id"].as_str().unwrap_or_default().to_owned());

    if after {
        let id = existing.expect("the restart check document was written before the restart");
        let latest = ok(&client, "get_document", json!({"document_id": id}))
            .await
            .unwrap();
        assert_eq!(latest["document"]["revision"], 2);
        assert!(
            latest["markdown"]
                .as_str()
                .unwrap()
                .ends_with("---\nsecond\n")
        );
        let first = ok(
            &client,
            "get_document",
            json!({"document_id": id, "version": 1}),
        )
        .await
        .unwrap();
        assert!(
            first["markdown"]
                .as_str()
                .unwrap()
                .ends_with("---\nfirst\n")
        );
    } else if existing.is_none() {
        let created = ok(
            &client,
            "new_document",
            json!({"project": t.project, "title": TITLE, "content": "first\n"}),
        )
        .await
        .unwrap();
        ok(
            &client,
            "update_document",
            json!({"document_id": created["document_id"], "content": "second\n", "expected_revision": 1}),
        )
        .await
        .unwrap();
    }
    client.cancel().await.unwrap();
}
