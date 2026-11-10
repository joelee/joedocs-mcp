//! End-to-end MCP tests: an in-process server on a loopback port, driven by
//! rmcp's own Streamable HTTP client, plus raw HTTP checks of the
//! authentication, `Host` and `Origin` rules, and one run of the real
//! `joedocs serve` binary.

use std::path::Path;
use std::sync::Arc;

mod common;

use joedocs::Config;
use joedocs::auth::CountingRandom;
use joedocs::clock::FixedClock;
use joedocs::history::GitCli;
use joedocs::mcp::{ServerOptions, Shared, serve};
use joedocs::store::{Actor, NewDocument, Paths, Settings, Store};
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use common::{Client, INIT, TestResult, call, connect, error, ok, post, status};

struct Server {
    _dir: TempDir,
    root: std::path::PathBuf,
    addr: std::net::SocketAddr,
    writer: String,
    reader: String,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

fn store(root: &Path) -> Result<Store, joedocs::store::Error> {
    let paths = Paths {
        home: root.join("vault"),
        state_dir: root.join("state"),
    };
    let git = GitCli::new(paths.git_dir(), &paths.home);
    Store::init(&paths, &git)?;
    Store::open(
        paths,
        Settings::from_config(&Config::default()),
        Box::new(git),
        Arc::new(FixedClock::at(1_793_000_000)),
    )
}

/// A server with projects `joedocs` (JD) and `secret` (SC, holding SC-1),
/// a `laptop` key with `joedocs:write` and a `viewer` key with
/// `joedocs:read`.
async fn start() -> TestResult<Server> {
    let dir = TempDir::new()?;
    let root = dir.path().to_owned();
    let mut store = store(&root)?;
    store.add_project("joedocs", "JD", Some("JoeDocs"))?;
    store.add_project("secret", "SC", None)?;
    store.create(
        &Actor::new("admin", "test"),
        NewDocument {
            project: "secret".into(),
            title: "Hidden".into(),
            content: "x".into(),
            ..NewDocument::default()
        },
    )?;
    let writer = store.create_key("laptop", &["joedocs:write".parse()?], &CountingRandom(0))?;
    let reader = store.create_key("viewer", &["joedocs:read".parse()?], &CountingRandom(100))?;

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let (stop, stopped) = oneshot::channel::<()>();
    let options = ServerOptions::from_config(&Config::default());
    let task = tokio::spawn(async move {
        serve(listener, Shared::new(store), &options, async {
            stopped.await.ok();
        })
        .await
    });
    Ok(Server {
        _dir: dir,
        root,
        addr,
        writer: writer.token.expose_secret().to_owned(),
        reader: reader.token.expose_secret().to_owned(),
        stop: Some(stop),
        task,
    })
}

impl Server {
    async fn client(&self, token: &str) -> TestResult<Client> {
        connect(&format!("http://{}/mcp", self.addr), token).await
    }

    async fn stop(mut self) -> TestResult<()> {
        if let Some(stop) = self.stop.take() {
            stop.send(()).ok();
        }
        Ok(self.task.await??)
    }
}

fn audit_log(root: &Path) -> TestResult<Vec<String>> {
    let conn = joedocs::db::open(&root.join("state/joedocs.db"))?;
    let mut stmt = conn.prepare(
        "SELECT tool, coalesce(document_id, '-'), outcome FROM audit_log ORDER BY rowid",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(format!(
            "{} {} {}",
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

#[tokio::test]
async fn tools_are_listed_with_schemas_and_hints() {
    let server = start().await.unwrap();
    let client = server.client(&server.writer).await.unwrap();

    let tools = client.list_all_tools().await.unwrap();
    let mut names: Vec<_> = tools.iter().map(|t| t.name.to_string()).collect();
    names.sort();
    assert_eq!(
        names,
        [
            "append_to_document",
            "archive_document",
            "archive_document_file",
            "change_document_front_matter",
            "diff_document",
            "edit_document",
            "get_document",
            "get_document_history",
            "list_document_files",
            "list_documents",
            "list_projects",
            "move_document",
            "new_document",
            "new_document_file",
            "rename_document",
            "restore_document_version",
            "search_documents",
            "update_document",
            "write_document_files",
        ]
    );
    for tool in &tools {
        assert!(
            tool.output_schema.is_some(),
            "{} has an output schema",
            tool.name
        );
        let read_only = tool.annotations.as_ref().and_then(|a| a.read_only_hint);
        let expected = [
            "diff_document",
            "get_document",
            "get_document_history",
            "list_document_files",
            "list_documents",
            "list_projects",
            "search_documents",
        ]
        .contains(&tool.name.as_ref());
        assert_eq!(read_only, Some(expected), "{}", tool.name);
    }

    client.cancel().await.unwrap();
    server.stop().await.unwrap();
}

/// The second half of the round trip: rename, front matter, list, old
/// versions and archive.
async fn rename_change_list_and_archive(client: &Client) -> TestResult<()> {
    let renamed = ok(
        client,
        "rename_document",
        json!({"document_id": "JD-1", "title": "MCP rollout"}),
    )
    .await?;
    assert_eq!(renamed["path"], "joedocs/plans/MCP rollout.md");
    assert_eq!(renamed["front_matter"]["aliases"], json!(["MCP plan"]));

    let changed = ok(
        client,
        "change_document_front_matter",
        json!({"document_id": "JD-1", "set": {"due": "2026-12-01"}, "remove": ["owner"],
               "expected_revision": 3}),
    )
    .await?;
    assert_eq!(changed["revision"], 4);
    assert!(changed["front_matter"].get("owner").is_none());

    let found = ok(
        client,
        "search_documents",
        json!({"project": "*", "query": "plan", "filters": {"due": {"gte": "2026-11-01"}}}),
    )
    .await?;
    assert_eq!(found["total"], 1, "{found}");
    let hit = &found["items"][0];
    assert_eq!(hit["id"], "JD-1");
    assert_eq!(hit["title"], "MCP rollout");
    assert_eq!(hit["snippet"], "# «Plan» v2\n");
    assert!(hit["score"].as_f64().is_some_and(|s| s > 0.0), "{hit}");
    let by_filter = ok(
        client,
        "search_documents",
        json!({"project": "joedocs", "type": "plans", "filters": {"tags": "mcp"}}),
    )
    .await?;
    assert_eq!(by_filter["items"][0]["id"], "JD-1");
    assert!(by_filter["items"][0].get("snippet").is_none());

    let listed = ok(client, "list_documents", json!({"project": "joedocs"})).await?;
    assert_eq!(listed["total"], 1);
    assert_eq!(listed["items"][0]["title"], "MCP rollout");
    assert_eq!(listed["next_start"], Value::Null);

    let old = ok(
        client,
        "get_document",
        json!({"document_id": "JD-1", "version": 1}),
    )
    .await?;
    assert_eq!(old["revision"]["revision"], 1);

    let archived = ok(
        client,
        "archive_document",
        json!({"document_id": "JD-1", "reason": "done"}),
    )
    .await?;
    assert_eq!(archived["revision"], 5);
    let gone = error(client, "get_document", json!({"document_id": "JD-1"})).await?;
    assert_eq!(gone["code"], "archived");
    let found = ok(
        client,
        "search_documents",
        json!({"project": "joedocs", "query": "plan"}),
    )
    .await?;
    assert_eq!(found["total"], 0);

    Ok(())
}

#[tokio::test]
async fn every_tool_round_trips() {
    let server = start().await.unwrap();
    let client = server.client(&server.writer).await.unwrap();

    let projects = ok(&client, "list_projects", json!({})).await.unwrap();
    assert_eq!(
        projects["projects"],
        json!([{"name": "joedocs", "key": "JD", "title": "JoeDocs", "permission": "write", "documents": 0}])
    );
    assert_eq!(projects["default_type"], "home");

    let created = ok(
        &client,
        "new_document",
        json!({
            "project": "joedocs",
            "type": "plans",
            "title": "MCP plan",
            "content": "# Plan\n",
            "front_matter": {"tags": ["mcp"], "owner": "joel"},
            "summary": "first draft"
        }),
    )
    .await
    .unwrap();
    assert_eq!(created["document_id"], "JD-1");
    assert_eq!(created["path"], "joedocs/plans/MCP plan.md");
    assert_eq!(created["front_matter"]["client"], "test-client");
    assert_eq!(created["front_matter"]["created_by"], "laptop");

    let got = ok(&client, "get_document", json!({"document_id": "JD-1"}))
        .await
        .unwrap();
    assert!(
        got["markdown"]
            .as_str()
            .unwrap()
            .ends_with("owner: joel\n---\n# Plan\n")
    );
    assert_eq!(got["document"]["type"], "plans");
    assert_eq!(got["revision"]["summary"], "Create JD-1: first draft");

    let stale = error(
        &client,
        "update_document",
        json!({"document_id": "JD-1", "content": "x", "expected_revision": 7}),
    )
    .await
    .unwrap();
    assert_eq!(stale["code"], "conflict");
    assert_eq!(stale["current_revision"], 1);

    let updated = ok(
        &client,
        "update_document",
        json!({"document_id": "JD-1", "content": "# Plan v2\n", "expected_revision": 1,
               "front_matter": {"status": "review"}}),
    )
    .await
    .unwrap();
    assert_eq!(updated["revision"], 2);
    assert_eq!(updated["front_matter"]["status"], "review");

    rename_change_list_and_archive(&client).await.unwrap();

    client.cancel().await.unwrap();
    let audit = audit_log(&server.root).unwrap();
    assert_eq!(
        audit,
        [
            "new_document JD-1 ok",
            "update_document JD-1 conflict",
            "update_document JD-1 ok",
            "rename_document JD-1 ok",
            "change_document_front_matter JD-1 ok",
            "archive_document JD-1 ok",
        ]
    );
    server.stop().await.unwrap();
}

#[tokio::test]
async fn multi_file_documents_round_trip() {
    let server = start().await.unwrap();
    let client = server.client(&server.writer).await.unwrap();
    let viewer = server.client(&server.reader).await.unwrap();

    let created = ok(
        &client,
        "new_document",
        json!({"project": "joedocs", "type": "plans", "title": "Roadmap",
               "content": "# Roadmap\n", "multifile": true}),
    )
    .await
    .unwrap();
    assert_eq!(created["path"], "joedocs/plans/Roadmap/Roadmap.md");
    assert_eq!(created.get("file"), None);

    let added = ok(
        &client,
        "new_document_file",
        json!({"document_id": "JD-1", "file": "backlog/item-1", "content": "# Item\n",
               "front_matter": {"status": "planned"}, "expected_revision": 1}),
    )
    .await
    .unwrap();
    assert_eq!(added["revision"], 2);
    assert_eq!(added["file"], "backlog/item-1");
    assert_eq!(added["path"], "joedocs/plans/Roadmap/backlog/item-1.md");
    assert_eq!(added["front_matter"]["document"], "JD-1");
    assert_eq!(added["front_matter"]["status"], "planned");

    let refused = error(
        &viewer,
        "new_document_file",
        json!({"document_id": "JD-1", "file": "x", "content": "x"}),
    )
    .await
    .unwrap();
    assert_eq!(refused["code"], "permission_denied");
    let bad = error(
        &client,
        "new_document_file",
        json!({"document_id": "JD-1", "file": "../x", "content": "x"}),
    )
    .await
    .unwrap();
    assert_eq!(
        (bad["code"].as_str(), bad["field"].as_str()),
        (Some("invalid_argument"), Some("file"))
    );

    multi_file_reads(&viewer).await.unwrap();

    let single = ok(
        &client,
        "new_document",
        json!({"project": "joedocs", "title": "Plain", "content": "x\n"}),
    )
    .await
    .unwrap();
    let plain = ok(
        &viewer,
        "get_document",
        json!({"document_id": single["document_id"]}),
    )
    .await
    .unwrap();
    assert_eq!(
        plain["document"].get("multifile"),
        None,
        "single-file output is unchanged"
    );
    assert_eq!(plain["document"].get("files"), None);
    assert_eq!(plain.get("files"), None);
    let found = ok(
        &viewer,
        "search_documents",
        json!({"project": "joedocs", "query": "plain"}),
    )
    .await
    .unwrap();
    let hit = &found["items"][0];
    assert_eq!(hit["id"], single["document_id"], "{found}");
    assert_eq!((hit.get("multifile"), hit.get("files")), (None, None));

    client.cancel().await.unwrap();
    viewer.cancel().await.unwrap();
    let audit = audit_log(&server.root).unwrap();
    assert_eq!(
        audit,
        [
            "new_document JD-1 ok",
            "new_document_file JD-1 ok",
            "new_document_file JD-1 permission_denied",
            "new_document_file JD-1 invalid_argument",
            "new_document JD-2 ok",
        ]
    );
    server.stop().await.unwrap();
}

#[tokio::test]
async fn file_changes_round_trip() {
    let server = start().await.unwrap();
    let client = server.client(&server.writer).await.unwrap();
    ok(
        &client,
        "new_document",
        json!({"project": "joedocs", "title": "Roadmap", "content": "x\n", "multifile": true}),
    )
    .await
    .unwrap();
    let written = ok(
        &client,
        "write_document_files",
        json!({"document_id": "JD-1", "expected_revision": 1, "files": [
            {"file": "a", "content": "a\n", "front_matter": {"status": "planned"}},
            {"file": "b/c", "content": "c\n"}
        ]}),
    )
    .await
    .unwrap();
    assert_eq!(written["revision"], 2);
    assert_eq!(
        written["files"],
        json!([
            {"file": "a", "path": "joedocs/home/Roadmap/a.md", "created": true},
            {"file": "b/c", "path": "joedocs/home/Roadmap/b/c.md", "created": true}
        ])
    );
    for (tool, args) in [
        (
            "update_document",
            json!({"file": "a", "content": "a2\n", "expected_revision": 2}),
        ),
        (
            "edit_document",
            json!({"file": "a", "edits": [{"old": "a2", "new": "a3"}]}),
        ),
        (
            "append_to_document",
            json!({"file": "b/c", "content": "more"}),
        ),
        (
            "change_document_front_matter",
            json!({"file": "a", "set": {"status": "done"}}),
        ),
        ("rename_document", json!({"file": "b/c", "new_file": "d"})),
        (
            "append_to_document",
            json!({"file": "Roadmap", "content": "main"}),
        ),
    ] {
        let mut args = args;
        args["document_id"] = json!("JD-1");
        ok(&client, tool, args).await.unwrap();
    }
    let item = ok(
        &client,
        "get_document",
        json!({"document_id": "JD-1", "file": "a"}),
    )
    .await
    .unwrap();
    assert!(
        item["markdown"]
            .as_str()
            .unwrap()
            .ends_with("status: done\n---\na3\n")
    );
    let main = ok(&client, "get_document", json!({"document_id": "JD-1"}))
        .await
        .unwrap();
    assert_eq!(main["files"], json!(["a", "d"]));
    assert!(main["markdown"].as_str().unwrap().ends_with("x\n\nmain\n"));

    file_refusals(&client).await.unwrap();
    client.cancel().await.unwrap();
    let audit = audit_log(&server.root).unwrap();
    assert_eq!(
        audit[1..],
        [
            "write_document_files JD-1 ok",
            "update_document JD-1 ok",
            "edit_document JD-1 ok",
            "append_to_document JD-1 ok",
            "change_document_front_matter JD-1 ok",
            "rename_document JD-1 ok",
            "append_to_document JD-1 ok",
            "update_document JD-1 conflict",
            "rename_document JD-1 invalid_argument",
        ]
    );
    server.stop().await.unwrap();
}

#[tokio::test]
async fn folder_operations_round_trip() {
    let server = start().await.unwrap();
    let client = server.client(&server.writer).await.unwrap();
    ok(
        &client,
        "new_document",
        json!({"project": "joedocs", "title": "Roadmap", "content": "x\n", "multifile": true}),
    )
    .await
    .unwrap();
    ok(
        &client,
        "write_document_files",
        json!({"document_id": "JD-1", "files": [
            {"file": "a", "content": "a\n"}, {"file": "b", "content": "b\n"}
        ]}),
    )
    .await
    .unwrap();
    let archived = ok(
        &client,
        "archive_document_file",
        json!({"document_id": "JD-1", "file": "a", "reason": "done", "expected_revision": 2}),
    )
    .await
    .unwrap();
    assert_eq!(archived["revision"], 3);
    assert_eq!(archived["file"], "a");
    assert_eq!(archived["path"], "joedocs/home/Roadmap/a.md");
    let renamed = ok(
        &client,
        "rename_document",
        json!({"document_id": "JD-1", "title": "Plan", "expected_revision": 3}),
    )
    .await
    .unwrap();
    assert_eq!(renamed["path"], "joedocs/home/Plan/Plan.md");
    let moved = ok(
        &client,
        "move_document",
        json!({"document_id": "JD-1", "type": "plans", "expected_revision": 4}),
    )
    .await
    .unwrap();
    assert_eq!(moved["path"], "joedocs/plans/Plan/Plan.md");
    let restored = ok(
        &client,
        "restore_document_version",
        json!({"document_id": "JD-1", "version": 2, "expected_revision": 5}),
    )
    .await
    .unwrap();
    assert_eq!(restored["revision"], 6);
    let files = ok(
        &client,
        "list_document_files",
        json!({"document_id": "JD-1"}),
    )
    .await
    .unwrap();
    let names: Vec<&str> = files["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["file"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Plan", "a", "b"]);
    let main = error(
        &client,
        "archive_document_file",
        json!({"document_id": "JD-1", "file": "Plan"}),
    )
    .await
    .unwrap();
    assert_eq!(
        (main["code"].as_str(), main["field"].as_str()),
        (Some("invalid_argument"), Some("file"))
    );
    ok(
        &client,
        "archive_document",
        json!({"document_id": "JD-1", "reason": "done"}),
    )
    .await
    .unwrap();
    client.cancel().await.unwrap();
    let audit = audit_log(&server.root).unwrap();
    assert_eq!(
        audit[2..],
        [
            "archive_document_file JD-1 ok",
            "rename_document JD-1 ok",
            "move_document JD-1 ok",
            "restore_document_version JD-1 ok",
            "archive_document_file JD-1 invalid_argument",
            "archive_document JD-1 ok",
        ]
    );
    server.stop().await.unwrap();
}

#[tokio::test]
async fn file_history_and_diffs_round_trip() {
    let server = start().await.unwrap();
    let client = server.client(&server.writer).await.unwrap();
    ok(
        &client,
        "new_document",
        json!({"project": "joedocs", "title": "Roadmap", "content": "x\n", "multifile": true}),
    )
    .await
    .unwrap();
    for (tool, args) in [
        ("new_document_file", json!({"file": "a", "content": "a\n"})),
        (
            "append_to_document",
            json!({"file": "a", "content": "more"}),
        ),
        ("rename_document", json!({"file": "a", "new_file": "b"})),
    ] {
        let mut args = args;
        args["document_id"] = json!("JD-1");
        ok(&client, tool, args).await.unwrap();
    }
    let history = ok(
        &client,
        "get_document_history",
        json!({"document_id": "JD-1", "file": "b"}),
    )
    .await
    .unwrap();
    let entries: Vec<(Value, Value)> = history["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["revision"].clone(), r["file"].clone()))
        .collect();
    assert_eq!(
        entries,
        [
            (json!(4), json!("b")),
            (json!(3), json!("a")),
            (json!(2), json!("a"))
        ]
    );
    let whole = ok(
        &client,
        "get_document_history",
        json!({"document_id": "JD-1"}),
    )
    .await
    .unwrap();
    assert_eq!(whole["total"], 4);
    assert_eq!(whole["revisions"][0].get("file"), None);

    let one = ok(
        &client,
        "diff_document",
        json!({"document_id": "JD-1", "file": "b", "from": 2}),
    )
    .await
    .unwrap();
    assert_eq!(one["files"], json!(["b"]));
    assert!(one["diff"].as_str().unwrap().contains("+more"), "{one}");
    let all = ok(
        &client,
        "diff_document",
        json!({"document_id": "JD-1", "from": 1}),
    )
    .await
    .unwrap();
    assert_eq!(all["files"], json!(["b"]));
    assert_eq!(all["changed"], true);

    let found = ok(
        &client,
        "search_documents",
        json!({"project": "joedocs", "query": "more", "document_id": "JD-1"}),
    )
    .await
    .unwrap();
    assert_eq!(found["total"], 1);
    assert_eq!(found["items"][0]["id"], "JD-1");
    assert_eq!(found["items"][0]["file"], "b");
    assert!(
        found["items"][0]["snippet"]
            .as_str()
            .unwrap()
            .contains("«more»")
    );
    let other = error(
        &client,
        "search_documents",
        json!({"project": "joedocs", "query": "more", "document_id": "SC-1"}),
    )
    .await
    .unwrap();
    assert_eq!(other["code"], "not_found");
    client.cancel().await.unwrap();
    server.stop().await.unwrap();
}

/// The refusals of the file round trip, on JD-1 at revision 8 with file
/// `a` last changed at revision 6.
async fn file_refusals(client: &Client) -> TestResult<()> {
    let stale = error(
        client,
        "update_document",
        json!({"document_id": "JD-1", "file": "a", "content": "z", "expected_revision": 2}),
    )
    .await?;
    assert_eq!(stale["code"], "conflict");
    assert_eq!(stale["file"], "a");
    assert_eq!(stale["file_revision"], 6);
    assert_eq!(stale["current_revision"], 8);
    let both = error(
        client,
        "rename_document",
        json!({"document_id": "JD-1", "title": "X", "file": "a", "new_file": "e"}),
    )
    .await?;
    assert_eq!(both["code"], "invalid_argument");
    Ok(())
}

/// The reading half of the multi-file round trip, on JD-1 holding the
/// file `backlog/item-1`.
async fn multi_file_reads(viewer: &Client) -> TestResult<()> {
    let files = ok(
        viewer,
        "list_document_files",
        json!({"document_id": "JD-1"}),
    )
    .await?;
    assert_eq!(files["revision"], 2);
    let names: Vec<(Value, Value, Value)> = files["files"]
        .as_array()
        .ok_or("no files")?
        .iter()
        .map(|f| (f["file"].clone(), f["main"].clone(), f["revision"].clone()))
        .collect();
    assert_eq!(
        names,
        [
            (json!("Roadmap"), json!(true), json!(1)),
            (json!("backlog/item-1"), json!(false), json!(2))
        ]
    );
    let earlier = ok(
        viewer,
        "list_document_files",
        json!({"document_id": "JD-1", "version": 1}),
    )
    .await?;
    assert_eq!(earlier["files"].as_array().map(Vec::len), Some(1));

    let item = ok(
        viewer,
        "get_document",
        json!({"document_id": "JD-1", "file": "backlog/item-1"}),
    )
    .await?;
    assert_eq!(item["file"], "backlog/item-1");
    let markdown = item["markdown"].as_str().ok_or("no markdown")?;
    assert!(
        markdown.ends_with("status: planned\n---\n# Item\n"),
        "{markdown}"
    );
    assert_eq!(item["document"]["multifile"], true);
    assert_eq!(item["document"]["files"], 1);
    let main = ok(viewer, "get_document", json!({"document_id": "JD-1"})).await?;
    assert_eq!(main.get("file"), None);
    assert_eq!(main["files"], json!(["backlog/item-1"]));
    let missing = error(
        viewer,
        "get_document",
        json!({"document_id": "JD-1", "file": "nope"}),
    )
    .await?;
    assert_eq!(
        (missing["code"].as_str(), missing["file"].as_str()),
        (Some("not_found"), Some("nope"))
    );

    let listed = ok(viewer, "list_documents", json!({"project": "joedocs"})).await?;
    assert_eq!(listed["items"][0]["multifile"], true);
    assert_eq!(listed["items"][0]["files"], 1);

    // Search hits, on the main file and on a file, carry the same count.
    let found = ok(
        viewer,
        "search_documents",
        json!({"project": "joedocs", "query": "roadmap OR item", "document_id": "JD-1"}),
    )
    .await?;
    let hits = found["items"].as_array().ok_or("no items")?;
    assert_eq!(hits.len(), 2, "{found}");
    for hit in hits {
        assert_eq!(
            (&hit["multifile"], &hit["files"]),
            (&json!(true), &json!(1)),
            "{hit}"
        );
    }
    Ok(())
}

/// `search_documents` covers only what the key can read.
async fn search_respects_grants(writer: &Client, reader: &Client) -> TestResult<()> {
    // Searching every project covers only the projects the key can read.
    let everywhere = ok(
        writer,
        "search_documents",
        json!({"project": "*", "query": "hidden OR shared"}),
    )
    .await?;
    assert_eq!(everywhere["items"][0]["id"], "JD-1");
    assert_eq!(everywhere["total"], 1);
    let read = ok(
        reader,
        "search_documents",
        json!({"project": "joedocs", "query": "shared"}),
    )
    .await?;
    assert_eq!(read["total"], 1);
    let hidden = error(
        writer,
        "search_documents",
        json!({"project": "secret", "query": "hidden"}),
    )
    .await?;
    assert_eq!(hidden["code"], "not_found");
    let bad = error(
        writer,
        "search_documents",
        json!({"project": "*", "query": "-only"}),
    )
    .await?;
    assert_eq!(
        (bad["code"].as_str(), bad["field"].as_str()),
        (Some("invalid_argument"), Some("query"))
    );
    Ok(())
}

/// JD-1's history and diff after an edit, an append and a move, as seen by
/// a key that may only read (and so may not append).
async fn check_history_and_diff(reader: &Client) -> TestResult<()> {
    let history = ok(
        reader,
        "get_document_history",
        json!({"document_id": "JD-1", "limit": 2}),
    )
    .await?;
    assert_eq!(history["total"], 4);
    assert_eq!(history["next_start"], 3);
    assert_eq!(history["revisions"][0]["action"], "move");
    assert_eq!(history["revisions"][1]["action"], "append");

    let diff = ok(
        reader,
        "diff_document",
        json!({"document_id": "JD-1", "from": 1, "context": 0}),
    )
    .await?;
    assert_eq!(
        (diff["to"].as_u64(), diff["changed"].as_bool()),
        (Some(4), Some(true))
    );
    let text = diff["diff"].as_str().ok_or("no diff")?;
    assert!(text.contains("-It fails.\n+It fails on start.\n"), "{text}");
    assert!(text.contains("-type: ideas\n+type: issues\n"), "{text}");
    let denied = error(
        reader,
        "append_to_document",
        json!({"document_id": "JD-1", "content": "x"}),
    )
    .await?;
    assert_eq!(denied["code"], "permission_denied");
    Ok(())
}

#[tokio::test]
async fn history_and_editing_tools_round_trip() {
    let server = start().await.unwrap();
    let client = server.client(&server.writer).await.unwrap();
    let reader = server.client(&server.reader).await.unwrap();
    ok(
        &client,
        "new_document",
        json!({"project": "joedocs", "type": "ideas", "title": "Issue", "content": "# Bug\n\nIt fails.\n"}),
    )
    .await
    .unwrap();

    let edited = ok(
        &client,
        "edit_document",
        json!({"document_id": "JD-1", "edits": [{"old": "It fails.", "new": "It fails on start."}],
               "expected_revision": 1, "summary": "more detail"}),
    )
    .await
    .unwrap();
    assert_eq!(edited["revision"], 2);
    let missing = error(
        &client,
        "edit_document",
        json!({"document_id": "JD-1", "edits": [{"old": "nope", "new": "x"}]}),
    )
    .await
    .unwrap();
    assert_eq!(
        (missing["code"].as_str(), missing["field"].as_str()),
        (Some("invalid_argument"), Some("edits"))
    );

    ok(
        &client,
        "append_to_document",
        json!({"document_id": "JD-1", "content": "## Comment\n\nReproduced."}),
    )
    .await
    .unwrap();
    let moved = ok(
        &client,
        "move_document",
        json!({"document_id": "JD-1", "type": "issues"}),
    )
    .await
    .unwrap();
    assert_eq!(moved["path"], "joedocs/issues/Issue.md");

    check_history_and_diff(&reader).await.unwrap();

    let restored = ok(
        &client,
        "restore_document_version",
        json!({"document_id": "JD-1", "version": 1, "expected_revision": 4}),
    )
    .await
    .unwrap();
    assert_eq!(restored["revision"], 5);
    assert_eq!(restored["front_matter"]["type"], "issues");
    let same = ok(
        &reader,
        "diff_document",
        json!({"document_id": "JD-1", "from": 1}),
    )
    .await
    .unwrap();
    assert!(
        same["diff"]
            .as_str()
            .unwrap()
            .contains("-revision: 1\n+revision: 5\n")
    );
    assert!(!same["diff"].as_str().unwrap().contains("fails"), "{same}");

    let hidden = error(
        &client,
        "get_document_history",
        json!({"document_id": "SC-1"}),
    )
    .await
    .unwrap();
    assert_eq!(hidden["code"], "not_found");

    client.cancel().await.unwrap();
    reader.cancel().await.unwrap();
    let audit = audit_log(&server.root).unwrap();
    assert_eq!(
        audit,
        [
            "new_document JD-1 ok",
            "edit_document JD-1 ok",
            "edit_document JD-1 invalid_argument",
            "append_to_document JD-1 ok",
            "move_document JD-1 ok",
            "append_to_document JD-1 permission_denied",
            "restore_document_version JD-1 ok",
        ]
    );
    server.stop().await.unwrap();
}

#[tokio::test]
async fn changes_that_change_nothing_are_refused() {
    let server = start().await.unwrap();
    let client = server.client(&server.writer).await.unwrap();
    ok(
        &client,
        "new_document",
        json!({"project": "joedocs", "title": "Note", "content": "Same.\n",
               "front_matter": {"owner": "joel"}}),
    )
    .await
    .unwrap();

    let refusals = [
        (
            "edit_document",
            json!({"document_id": "JD-1", "edits": [{"old": "Same.", "new": "Same."}]}),
            "edits",
        ),
        (
            "update_document",
            json!({"document_id": "JD-1", "content": "Same.\n", "expected_revision": 1,
                   "front_matter": {"owner": "joel"}}),
            "content",
        ),
        (
            "change_document_front_matter",
            json!({"document_id": "JD-1", "set": {"owner": "joel"}}),
            "front_matter",
        ),
    ];
    for (tool, args, field) in refusals {
        let refused = error(&client, tool, args).await.unwrap();
        assert_eq!(
            (refused["code"].as_str(), refused["field"].as_str()),
            (Some("invalid_argument"), Some(field)),
            "{tool}: {refused}"
        );
    }
    let current = ok(&client, "get_document", json!({"document_id": "JD-1"}))
        .await
        .unwrap();
    assert_eq!(current["document"]["revision"], 1, "{current}");

    client.cancel().await.unwrap();
    server.stop().await.unwrap();
}

#[tokio::test]
async fn grants_limit_what_a_key_can_see_and_change() {
    let server = start().await.unwrap();
    let writer = server.client(&server.writer).await.unwrap();
    let reader = server.client(&server.reader).await.unwrap();

    ok(
        &writer,
        "new_document",
        json!({"project": "joedocs", "title": "Shared", "content": "x"}),
    )
    .await
    .unwrap();

    let projects = ok(&reader, "list_projects", json!({})).await.unwrap();
    assert_eq!(projects["projects"][0]["permission"], "read");
    ok(&reader, "get_document", json!({"document_id": "JD-1"}))
        .await
        .unwrap()
        .get("markdown")
        .unwrap();
    let denied = error(
        &reader,
        "update_document",
        json!({"document_id": "JD-1", "content": "y", "expected_revision": 1}),
    )
    .await
    .unwrap();
    assert_eq!(denied["code"], "permission_denied");
    let denied = error(
        &reader,
        "new_document",
        json!({"project": "joedocs", "title": "No", "content": "x"}),
    )
    .await
    .unwrap();
    assert_eq!(denied["code"], "permission_denied");

    for (tool, args) in [
        ("get_document", json!({"document_id": "SC-1"})),
        (
            "archive_document",
            json!({"document_id": "SC-1", "reason": "x"}),
        ),
        ("list_documents", json!({"project": "secret"})),
        (
            "new_document",
            json!({"project": "secret", "title": "t", "content": "c"}),
        ),
        ("get_document", json!({"document_id": "ZZ-1"})),
    ] {
        let hidden = error(&writer, tool, args).await.unwrap();
        assert_eq!(hidden["code"], "not_found", "{tool}");
    }
    search_respects_grants(&writer, &reader).await.unwrap();

    let invalid = error(&writer, "get_document", json!({"document_id": "nonsense"}))
        .await
        .unwrap();
    assert_eq!(invalid["code"], "invalid_argument");
    assert_eq!(invalid["field"], "document_id");

    let unknown = call(
        &writer,
        "get_document",
        json!({"document_id": "JD-1", "typo": 1}),
    )
    .await
    .unwrap();
    assert_eq!(
        unknown.is_error,
        Some(true),
        "unknown arguments are rejected"
    );

    writer.cancel().await.unwrap();
    reader.cancel().await.unwrap();
    let audit = audit_log(&server.root).unwrap();
    assert!(
        audit.contains(&"update_document JD-1 permission_denied".to_owned()),
        "{audit:?}"
    );
    assert!(
        audit.contains(&"archive_document SC-1 not_found".to_owned()),
        "{audit:?}"
    );
    server.stop().await.unwrap();
}

#[tokio::test]
async fn http_requests_need_a_valid_key_host_and_origin() {
    let server = start().await.unwrap();
    let addr = server.addr;
    let host = addr.to_string();
    let auth = format!("Authorization: Bearer {}\r\n", server.writer);

    let health = format!("GET /health HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    assert_eq!(status(&host, &health).await.unwrap(), 200);
    assert_eq!(status(&host, &post(&host, "", INIT)).await.unwrap(), 401);
    let wrong = format!(
        "Authorization: Bearer jd_{}_{}\r\n",
        "0".repeat(12),
        "1".repeat(64)
    );
    assert_eq!(
        status(&host, &post(&host, &wrong, INIT)).await.unwrap(),
        401
    );
    assert_eq!(
        status(&host, &post(&host, "Authorization: Basic eDp5\r\n", INIT))
            .await
            .unwrap(),
        401
    );
    assert_eq!(status(&host, &post(&host, &auth, INIT)).await.unwrap(), 200);

    let evil_origin = format!("{auth}Origin: http://evil.example\r\n");
    assert_eq!(
        status(&host, &post(&host, &evil_origin, INIT))
            .await
            .unwrap(),
        403
    );
    assert_eq!(
        status(&host, &post("evil.example", &auth, INIT))
            .await
            .unwrap(),
        403
    );

    {
        let mut store = store(&server.root).unwrap();
        store.revoke("laptop").unwrap();
    }
    assert_eq!(status(&host, &post(&host, &auth, INIT)).await.unwrap(), 401);
    server.stop().await.unwrap();
}

/// `joedocs <args>` on the server's store, returning its standard output.
fn admin_output(root: &Path, args: &[&str]) -> TestResult<String> {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_joedocs"));
    cmd.env_clear()
        .env("HOME", root)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("JOEDOCS__STORAGE__HOME", root.join("vault"))
        .env("JOEDOCS__STORAGE__STATE_DIR", root.join("state"))
        .current_dir(root);
    if let Ok(profile) = std::env::var("LLVM_PROFILE_FILE") {
        cmd.env("LLVM_PROFILE_FILE", profile);
    }
    let output = cmd.args(args).output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

#[tokio::test]
async fn admin_json_matches_the_mcp_results() -> TestResult<()> {
    let server = start().await?;
    let client = server.client(&server.writer).await?;
    for (title, doc_type, multifile) in [
        ("One", "plans", false),
        ("Two", "notes", true),
        ("Three", "plans", false),
    ] {
        ok(
            &client,
            "new_document",
            json!({"project": "joedocs", "title": title, "type": doc_type,
                   "content": "body", "multifile": multifile}),
        )
        .await?;
    }
    for (tool, args, cli) in [
        (
            "list_documents",
            json!({"project": "joedocs"}),
            vec!["list", "-p", "joedocs", "-j"],
        ),
        (
            "search_documents",
            json!({"project": "joedocs", "query": "body"}),
            vec!["search", "body", "-p", "joedocs", "-j"],
        ),
        (
            "search_documents",
            json!({"project": "joedocs", "type": "plans", "filters": {"status": "draft"}, "limit": 1}),
            vec![
                "search", "-p", "joedocs", "-t", "plans", "-s", "draft", "-n", "1", "-j",
            ],
        ),
        (
            "list_documents",
            json!({"project": "joedocs", "type": "plans", "sort": "title", "limit": 1}),
            vec![
                "list", "-p", "joedocs", "-t", "plans", "--sort", "title", "-n", "1", "-j",
            ],
        ),
    ] {
        let mcp = ok(&client, tool, args).await?;
        let admin: Value = serde_json::from_str(&admin_output(&server.root, &cli)?)?;
        assert_eq!(admin, mcp, "{cli:?}");
    }
    client.cancel().await?;
    server.stop().await
}

#[test]
fn the_serve_command_runs_until_sigterm() {
    use std::io::{BufRead as _, BufReader};
    use std::process::{Command, Stdio};

    let dir = TempDir::new().unwrap();
    let env = |cmd: &mut Command| {
        cmd.env_clear()
            .env("HOME", dir.path())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("JOEDOCS__STORAGE__HOME", dir.path().join("vault"))
            .env("JOEDOCS__STORAGE__STATE_DIR", dir.path().join("state"))
            .env("JOEDOCS__SERVER__BIND", "127.0.0.1:0")
            .current_dir(dir.path());
        if let Ok(profile) = std::env::var("LLVM_PROFILE_FILE") {
            cmd.env("LLVM_PROFILE_FILE", profile);
        }
    };
    let binary = env!("CARGO_BIN_EXE_joedocs");
    let mut init = Command::new(binary);
    env(&mut init);
    assert!(init.arg("init").output().unwrap().status.success());
    let mut add = Command::new(binary);
    env(&mut add);
    let added = add
        .args(["project", "add", "joedocs", "--key", "JD"])
        .output()
        .unwrap();
    assert!(added.status.success());
    // serve rewrites a missing INDEX.md on start.
    let index = dir.path().join("vault/joedocs/INDEX.md");
    std::fs::remove_file(&index).unwrap();

    let mut serve = Command::new(binary);
    env(&mut serve);
    let mut child = serve
        .arg("serve")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(
        line.starts_with("JoeDocs MCP endpoint: http://127.0.0.1:"),
        "{line}"
    );
    assert!(index.is_file(), "serve wrote INDEX.md before listening");
    let addr: std::net::SocketAddr = line
        .trim()
        .trim_start_matches("JoeDocs MCP endpoint: http://")
        .trim_end_matches("/mcp")
        .parse()
        .unwrap();
    let health = format!("GET /health HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    assert_eq!(
        runtime
            .block_on(status(&addr.to_string(), &health))
            .unwrap(),
        200
    );

    let killed = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    assert!(child.wait().unwrap().success(), "graceful shutdown exits 0");
}

#[test]
fn serve_reports_an_unusable_address() {
    let dir = TempDir::new().unwrap();
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let cmd = || {
        let mut cmd = assert_cmd::cargo::cargo_bin_cmd!();
        cmd.env_clear()
            .env("HOME", dir.path())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("JOEDOCS__STORAGE__HOME", dir.path().join("vault"))
            .env("JOEDOCS__STORAGE__STATE_DIR", dir.path().join("state"))
            .env(
                "JOEDOCS__SERVER__BIND",
                taken.local_addr().unwrap().to_string(),
            )
            .current_dir(dir.path());
        if let Ok(profile) = std::env::var("LLVM_PROFILE_FILE") {
            cmd.env("LLVM_PROFILE_FILE", profile);
        }
        cmd
    };
    cmd().arg("init").assert().success();
    cmd()
        .arg("serve")
        .assert()
        .code(69)
        .stderr(predicates::str::contains("cannot serve on 127.0.0.1:"));
}
