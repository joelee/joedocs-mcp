//! `reindex` rebuilds the index from the history: identically after normal
//! use, after a write whose database step was lost, and after losing the
//! database altogether.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use joedocs::Config;
use joedocs::clock::FixedClock;
use joedocs::document::front_matter::Fields;
use joedocs::history::{Commit, GitCli, History as _};
use joedocs::store::{Actor, Error, NewDocument, Paths, Settings, Store, Update};
use rusqlite::types::ValueRef;
use serde_json::json;
use tempfile::TempDir;

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

fn paths(root: &Path) -> Paths {
    Paths {
        home: root.join("vault"),
        state_dir: root.join("state"),
    }
}

fn git(root: &Path) -> GitCli {
    let paths = paths(root);
    GitCli::new(paths.git_dir(), &paths.home)
}

fn open(root: &Path) -> Result<Store, Error> {
    let paths = paths(root);
    Store::init(&paths, &git(root))?;
    Store::open(
        paths,
        Settings::from_config(&Config::default()),
        Box::new(git(root)),
        Arc::new(FixedClock::at(1_793_000_000)),
    )
}

fn actor() -> Actor {
    Actor::new("laptop", "claude-code")
}

fn update(id: &str, content: &str, expected: u64) -> Update {
    Update {
        id: id.into(),
        content: content.into(),
        expected_revision: expected,
        ..Update::default()
    }
}

/// Two projects and four documents with every kind of change, one archived.
fn populate(store: &mut Store) -> TestResult<()> {
    store.add_project("joedocs", "JD", Some("JoeDocs"))?;
    store.add_project("infra", "IN", None)?;
    for (project, title) in [
        ("joedocs", "Plan"),
        ("joedocs", "Idea"),
        ("infra", "Cache"),
        ("joedocs", "Note"),
    ] {
        store.create(
            &actor(),
            NewDocument {
                project: project.into(),
                doc_type: Some("plans".into()),
                title: title.into(),
                content: format!("{title} body\n"),
                front_matter: [("tags".to_owned(), json!(["a", "b"]))]
                    .into_iter()
                    .collect::<Fields>(),
                summary: Some(format!("first {title}")),
                ..NewDocument::default()
            },
        )?;
    }
    store.update(&actor(), update("JD-1", "Plan v2\n", 1))?;
    store.rename(&actor(), "JD-1", "Plan renamed", Some(2))?;
    let set: Fields = [("owner".to_owned(), json!("joel"))].into_iter().collect();
    store.change_front_matter(&actor(), "JD-2", &set, &[], None)?;
    store.archive(&actor(), "JD-3", "superseded by IN-1")?;
    Ok(())
}

fn db_files(root: &Path) -> [PathBuf; 3] {
    let db = paths(root).db();
    let with = |suffix: &str| PathBuf::from(format!("{}{suffix}", db.display()));
    [db.clone(), with("-wal"), with("-shm")]
}

/// Every index table, as text, for comparing two states.
fn snapshot(root: &Path) -> TestResult<Vec<String>> {
    let conn = joedocs::db::open(&paths(root).db())?;
    let mut lines = Vec::new();
    for (table, sql) in [
        ("projects", "SELECT * FROM projects ORDER BY id"),
        ("documents", "SELECT * FROM documents ORDER BY id"),
        (
            "revisions",
            "SELECT * FROM revisions ORDER BY document_id, revision",
        ),
        (
            "doc_fields",
            "SELECT * FROM doc_fields ORDER BY document_id, key, value",
        ),
        (
            "doc_fts",
            "SELECT rowid, title, body FROM doc_fts ORDER BY rowid",
        ),
        ("meta", "SELECT * FROM meta ORDER BY key"),
    ] {
        let mut stmt = conn.prepare(sql)?;
        let columns = stmt.column_count();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let mut line = table.to_owned();
            for i in 0..columns {
                let value = match row.get_ref(i)? {
                    ValueRef::Text(text) => String::from_utf8_lossy(text).into_owned(),
                    other => format!("{other:?}"),
                };
                line.push_str(" | ");
                line.push_str(&value);
            }
            lines.push(line);
        }
    }
    Ok(lines)
}

#[test]
fn a_rebuilt_index_is_identical() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    populate(&mut store).unwrap();
    assert!(!store.needs_reindex().unwrap());
    let before = snapshot(dir.path()).unwrap();
    assert!(
        before.iter().any(|l| l.starts_with("doc_fts")),
        "{before:?}"
    );

    let report = store.reindex().unwrap();
    assert_eq!(
        (report.documents, report.archived, report.revisions),
        (3, 1, 8)
    );
    assert_eq!(report.skipped_commits, 0);
    assert!(report.projects_created.is_empty() && report.drifted.is_empty());
    assert_eq!(snapshot(dir.path()).unwrap(), before);
    assert!(!store.needs_reindex().unwrap());
}

#[test]
fn an_index_left_behind_by_an_interrupted_write_catches_up() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    populate(&mut store).unwrap();
    drop(store);
    // The database as it was before the next write reached it.
    let saved: Vec<Option<Vec<u8>>> = db_files(dir.path())
        .iter()
        .map(|f| fs::read(f).ok())
        .collect();
    let mut store = open(dir.path()).unwrap();
    store
        .update(&actor(), update("JD-2", "Idea v2\n", 2))
        .unwrap();
    drop(store);
    for (file, bytes) in db_files(dir.path()).iter().zip(saved) {
        match bytes {
            Some(bytes) => fs::write(file, bytes).unwrap(),
            None => fs::remove_file(file).unwrap_or_default(),
        }
    }

    let mut store = open(dir.path()).unwrap();
    assert!(store.needs_reindex().unwrap());
    let refused = store
        .update(&actor(), update("JD-2", "x\n", 3))
        .unwrap_err();
    assert!(matches!(refused, Error::OutOfSync { .. }), "{refused}");
    assert!(refused.to_string().contains("joedocs reindex"), "{refused}");

    store.reindex().unwrap();
    assert!(!store.needs_reindex().unwrap());
    let fetched = store.get("JD-2", None).unwrap();
    assert_eq!(fetched.document.revision, 3);
    assert!(fetched.markdown.ends_with("---\nIdea v2\n"));
    store
        .update(&actor(), update("JD-2", "Idea v3\n", 3))
        .unwrap();
}

#[test]
fn a_lost_database_is_rebuilt_with_its_projects() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    populate(&mut store).unwrap();
    drop(store);
    for file in db_files(dir.path()) {
        fs::remove_file(file).unwrap_or_default();
    }

    let mut store = open(dir.path()).unwrap();
    assert_eq!(store.projects().unwrap(), Vec::new());
    assert!(store.needs_reindex().unwrap());
    let report = store.reindex().unwrap();
    assert_eq!(
        report.projects_created,
        [
            ("joedocs".to_owned(), "JD".to_owned()),
            ("infra".to_owned(), "IN".to_owned())
        ]
    );
    assert_eq!(report.documents + report.archived, 4);
    let fetched = store.get("JD-1", Some(1)).unwrap();
    assert_eq!(fetched.revision.summary, "Create JD-1: first Plan");
    assert_eq!(
        store.get("JD-1", None).unwrap().document.title,
        "Plan renamed"
    );
    assert!(matches!(store.get("JD-3", None), Err(Error::Archived(_))));

    // Numbering carries on after the highest recovered ID.
    let created = store
        .create(
            &actor(),
            NewDocument {
                project: "joedocs".into(),
                title: "After".into(),
                content: "x".into(),
                ..NewDocument::default()
            },
        )
        .unwrap();
    assert_eq!(created.id, "JD-4");
}

#[test]
fn foreign_commits_are_skipped_and_drifted_files_reported() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    populate(&mut store).unwrap();
    let home = paths(dir.path()).home;
    fs::write(home.join("stray.md"), "x").unwrap();
    git(dir.path())
        .commit(&Commit {
            paths: vec!["stray.md".into()],
            author: "someone".into(),
            at: jiff::Timestamp::UNIX_EPOCH,
            message: "by hand".into(),
        })
        .unwrap();
    fs::write(home.join("joedocs/plans/Idea.md"), "edited by hand").unwrap();

    assert!(store.needs_reindex().unwrap());
    let report = store.reindex().unwrap();
    assert_eq!(report.skipped_commits, 1);
    assert_eq!(
        report.drifted,
        [("JD-2".to_owned(), "joedocs/plans/Idea.md".to_owned())]
    );
    assert!(!store.needs_reindex().unwrap());
    let refused = store.update(&actor(), update("JD-2", "y", 2)).unwrap_err();
    assert!(matches!(refused, Error::Integrity { .. }), "{refused}");
}

fn commit_raw(root: &Path, message: &str) -> TestResult<()> {
    let home = paths(root).home;
    fs::write(home.join("raw.md"), message)?;
    git(root).commit(&Commit {
        paths: vec!["raw.md".into()],
        author: "someone".into(),
        at: jiff::Timestamp::UNIX_EPOCH,
        message: message.into(),
    })?;
    Ok(())
}

#[test]
fn a_history_that_cannot_be_replayed_leaves_the_index_alone() {
    let cases = [
        (
            "x\n\nJoeDocs-Document: JD-9\nJoeDocs-Revision: 2\nJoeDocs-Action: update\n",
            "JD-9 revision 2 found where revision 1 was expected",
        ),
        (
            "x\n\nJoeDocs-Document: nonsense\nJoeDocs-Revision: 1\nJoeDocs-Action: create\n",
            "nonsense",
        ),
        (
            "x\n\nJoeDocs-Document: JD-9\nJoeDocs-Revision: one\nJoeDocs-Action: create\n",
            "not a number",
        ),
        (
            "x\n\nJoeDocs-Document: JD-9\nJoeDocs-Revision: 1\n",
            "no JoeDocs-Action trailer",
        ),
        (
            "x\n\nJoeDocs-Document: JD-9\nJoeDocs-Revision: 1\nJoeDocs-Action: archive\n",
            "no file for JD-9",
        ),
        (
            "x\n\nJoeDocs-Document: JD-9\nJoeDocs-Revision: 1\nJoeDocs-Action: create\n",
            "front matter",
        ),
    ];
    for (message, expected) in cases {
        let dir = TempDir::new().unwrap();
        let mut store = open(dir.path()).unwrap();
        populate(&mut store).unwrap();
        let before = snapshot(dir.path()).unwrap();
        commit_raw(dir.path(), message).unwrap();
        let err = store.reindex().unwrap_err();
        assert!(matches!(err, Error::Rebuild { .. }), "{err}");
        assert!(err.to_string().contains(expected), "{expected}: {err}");
        assert_eq!(snapshot(dir.path()).unwrap(), before, "{expected}");
    }
}

#[test]
fn an_index_from_an_older_build_needs_a_rebuild() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    assert!(!store.needs_reindex().unwrap());
    let conn = joedocs::db::open(&paths(dir.path()).db()).unwrap();
    conn.execute("DELETE FROM meta WHERE key = 'index_format'", [])
        .unwrap();
    assert!(store.needs_reindex().unwrap());
    let report = store.reindex().unwrap();
    assert_eq!(report, joedocs::store::Reindexed::default());
    assert!(!store.needs_reindex().unwrap());
}
