//! Drives the store through a document's whole life against a temporary
//! vault and real git, then checks the vault, the history and the index
//! agree, including when a file is edited behind the store's back.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use joedocs::Config;
use joedocs::clock::FixedClock;
use joedocs::document::front_matter::{Fields, parse};
use joedocs::history::GitCli;
use joedocs::store::{
    Actor, Error, ListRequest, NewDocument, Paths, Settings, Sort, Store, Update,
};
use serde_json::json;
use tempfile::TempDir;

fn open(root: &Path) -> Result<Store, Error> {
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

/// `git log` of the history, newest first, as `subject|action|revision`.
fn log(root: &Path) -> std::io::Result<Vec<String>> {
    let output = Command::new("git")
        .arg("--git-dir")
        .arg(root.join("state/history.git"))
        .args([
            "log",
            "--format=%s|%(trailers:key=JoeDocs-Action,valueonly,separator=)|%(trailers:key=JoeDocs-Revision,valueonly,separator=)",
        ])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Every file under `dir`, relative to it and sorted.
fn vault_files(dir: &Path) -> std::io::Result<Vec<String>> {
    let mut files = Vec::new();
    let mut pending = vec![(dir.to_owned(), String::new())];
    while let Some((dir, prefix)) = pending.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
            if entry.file_type()?.is_dir() {
                pending.push((entry.path(), format!("{name}/")));
            } else {
                files.push(name);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn fields(value: &serde_json::Value) -> Fields {
    value
        .as_object()
        .map(|map| map.clone().into_iter().collect())
        .unwrap_or_default()
}

#[test]
fn a_document_lifecycle_is_recorded_in_files_history_and_index() {
    let root = TempDir::new().unwrap();
    let mut store = open(root.path()).unwrap();
    store.add_project("joedocs", "JD", Some("JoeDocs")).unwrap();
    let laptop = Actor::new("laptop", "claude-code");
    let desktop = Actor::new("desktop", "codex");

    let created = store
        .create(
            &laptop,
            NewDocument {
                project: "joedocs".into(),
                doc_type: Some("specs".into()),
                title: "Search query language".into(),
                content: "# Grammar\n\n```mermaid\nflowchart LR\n  a --> b\n```\n".into(),
                front_matter: fields(&json!({"tags": ["search"]})),
                ..NewDocument::default()
            },
        )
        .unwrap();
    store
        .update(
            &desktop,
            Update {
                id: created.id.clone(),
                content: "# Grammar v2\n".into(),
                expected_revision: 1,
                summary: Some("rewrite".into()),
                ..Update::default()
            },
        )
        .unwrap();
    store
        .rename(&laptop, &created.id, "Search syntax", Some(2))
        .unwrap();
    let changed = store
        .change_front_matter(
            &laptop,
            &created.id,
            &fields(&json!({"status": "approved", "approved_by": "joel"})),
            &[],
            Some(3),
        )
        .unwrap();
    assert_eq!(changed.path, "joedocs/specs/Search syntax.md");

    let file = std::fs::read_to_string(root.path().join("vault").join(&changed.path)).unwrap();
    let parsed = parse(&file).unwrap();
    assert_eq!(parsed.body, "# Grammar v2\n");
    let fm = &parsed.fields;
    assert_eq!(fm["id"], json!("JD-1"));
    assert_eq!(fm["revision"], json!(4));
    assert_eq!(fm["status"], json!("approved"));
    assert_eq!(fm["created_by"], json!("laptop"));
    assert_eq!(fm["aliases"], json!(["Search query language"]));
    assert_eq!(fm["approved_by"], json!("joel"));

    for version in 1..=4 {
        let fetched = store.get(&created.id, Some(version)).unwrap();
        assert_eq!(fetched.revision.revision, version);
        let at_version = parse(&fetched.markdown).unwrap();
        assert_eq!(at_version.fields["revision"], json!(version));
    }
    assert!(
        store
            .get(&created.id, Some(1))
            .unwrap()
            .markdown
            .contains("```mermaid\nflowchart LR\n  a --> b\n```\n"),
        "fenced blocks are kept as written"
    );

    store
        .archive(&desktop, &created.id, "replaced by the plan")
        .unwrap();
    assert_eq!(
        log(root.path()).unwrap(),
        [
            "Archive JD-1: replaced by the plan|archive|5",
            "Change front matter of JD-1|front_matter|4",
            "Rename JD-1: Search query language → Search syntax|rename|3",
            "Update JD-1: rewrite|update|2",
            "Create JD-1: Search query language|create|1",
        ]
    );
    let listing = store
        .list(&ListRequest {
            project: "joedocs".into(),
            doc_type: None,
            status: None,
            sort: Sort::default(),
            start: 1,
            limit: 10,
        })
        .unwrap();
    assert_eq!(listing.total, 0);
    assert_eq!(
        vault_files(&root.path().join("vault")).unwrap(),
        ["joedocs/INDEX.md"],
        "the vault holds only live documents and each project's INDEX.md"
    );
}

#[test]
fn a_file_edited_behind_the_stores_back_blocks_writes_but_not_reads() {
    let root = TempDir::new().unwrap();
    let mut store = open(root.path()).unwrap();
    store.add_project("joedocs", "JD", None).unwrap();
    let actor = Actor::new("laptop", "claude-code");
    let created = store
        .create(
            &actor,
            NewDocument {
                project: "joedocs".into(),
                title: "Plan".into(),
                content: "v1\n".into(),
                ..NewDocument::default()
            },
        )
        .unwrap();
    let path = root.path().join("vault").join(&created.path);
    std::fs::write(&path, "rewritten in Obsidian\n").unwrap();

    let err = store
        .update(
            &actor,
            Update {
                id: created.id.clone(),
                content: "v2\n".into(),
                expected_revision: 1,
                ..Update::default()
            },
        )
        .unwrap_err();
    assert!(matches!(err, Error::Integrity { .. }), "{err}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "rewritten in Obsidian\n"
    );
    assert_eq!(log(root.path()).unwrap().len(), 1);
    assert!(
        store
            .get(&created.id, None)
            .unwrap()
            .markdown
            .ends_with("---\nv1\n")
    );
}
