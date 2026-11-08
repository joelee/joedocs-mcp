//! Multi-file documents: a folder of Markdown files under one document ID
//! and one revision sequence, with the folder `INDEX.md`, the history
//! trailers, `reindex` and `check`.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use joedocs::Config;
use joedocs::clock::FixedClock;
use joedocs::document::front_matter::Fields;
use joedocs::history::{GitCli, History as _};
use joedocs::store::{Actor, Error, NewDocument, NewFile, Paths, Settings, Store};
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

fn open_with(root: &Path, settings: Settings) -> Result<Store, Error> {
    let paths = paths(root);
    Store::init(&paths, &git(root))?;
    Store::open(
        paths,
        settings,
        Box::new(git(root)),
        Arc::new(FixedClock::at(1_793_000_000)),
    )
}

fn open(root: &Path) -> Result<Store, Error> {
    open_with(root, Settings::from_config(&Config::default()))
}

fn actor() -> Actor {
    Actor::new("laptop", "claude-code")
}

fn fields(value: serde_json::Value) -> Fields {
    match value {
        serde_json::Value::Object(map) => map.into_iter().collect(),
        _ => Fields::new(),
    }
}

/// Project `joedocs` with JD-1, a multi-file "Roadmap", and JD-2, a
/// single-file "Plan".
fn roadmap(store: &mut Store) -> TestResult<()> {
    store.add_project("joedocs", "JD", Some("JoeDocs"))?;
    let changed = store.create(
        &actor(),
        NewDocument {
            project: "joedocs".into(),
            doc_type: Some("plans".into()),
            title: "Roadmap".into(),
            content: "# Roadmap\n".into(),
            multifile: true,
            ..NewDocument::default()
        },
    )?;
    assert_eq!(changed.path, "joedocs/plans/Roadmap/Roadmap.md");
    store.create(
        &actor(),
        NewDocument {
            project: "joedocs".into(),
            doc_type: Some("plans".into()),
            title: "Plan".into(),
            content: "plan\n".into(),
            ..NewDocument::default()
        },
    )?;
    Ok(())
}

fn new_file(file: &str, content: &str, expected: Option<u64>) -> NewFile {
    NewFile {
        document_id: "JD-1".into(),
        file: file.into(),
        content: content.into(),
        expected_revision: expected,
        ..NewFile::default()
    }
}

/// Every index table, as text, for comparing two states.
fn snapshot(root: &Path) -> TestResult<Vec<String>> {
    let conn = joedocs::db::open(&paths(root).db())?;
    let mut lines = Vec::new();
    for (table, sql) in [
        ("documents", "SELECT * FROM documents ORDER BY id"),
        (
            "revisions",
            "SELECT * FROM revisions ORDER BY document_id, revision",
        ),
        ("document_files", "SELECT * FROM document_files ORDER BY id"),
        (
            "revision_files",
            "SELECT * FROM revision_files ORDER BY document_id, revision, main, path",
        ),
        (
            "doc_fields",
            "SELECT * FROM doc_fields ORDER BY document_id, file_id, key, value",
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

fn assert_rebuilds_identically(root: &Path, store: &mut Store) -> TestResult<()> {
    let before = snapshot(root)?;
    let report = store.reindex()?;
    assert_eq!(report.drifted, Vec::<(String, String)>::new());
    assert_eq!(snapshot(root)?, before);
    Ok(())
}

#[test]
fn files_join_a_multi_file_document_under_its_id() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap(&mut store).unwrap();
    let main = dir.path().join("vault/joedocs/plans/Roadmap/Roadmap.md");
    let main_bytes = fs::read(&main).unwrap();

    let mut item = new_file("backlog/item-1", "# Item 1\n", Some(1));
    item.front_matter = fields(json!({"status": "planned", "priority": 2}));
    let changed = store.create_file(&actor(), &item).unwrap();
    assert_eq!(
        (
            changed.revision,
            changed.path.as_str(),
            changed.file.as_deref()
        ),
        (
            2,
            "joedocs/plans/Roadmap/backlog/item-1.md",
            Some("backlog/item-1")
        )
    );
    assert_eq!(changed.front_matter["status"], json!("planned"));
    // Created against revision 1 again: a new file conflicts with nothing.
    let notes = store
        .create_file(&actor(), &new_file("notes.md", "notes\n", Some(1)))
        .unwrap();
    assert_eq!(notes.revision, 3);
    assert_eq!(
        fs::read(&main).unwrap(),
        main_bytes,
        "the main file is not rewritten"
    );

    let text = fs::read_to_string(
        dir.path()
            .join("vault/joedocs/plans/Roadmap/backlog/item-1.md"),
    )
    .unwrap();
    assert!(
        text.starts_with("---\ndocument: JD-1\nrevision: 2\nupdated: "),
        "{text}"
    );
    assert!(text.ends_with("status: planned\n---\n# Item 1\n"), "{text}");

    let listed = store.list_files("JD-1", None).unwrap();
    let summary: Vec<(String, bool, u64)> = listed
        .iter()
        .map(|f| (f.file.clone(), f.main, f.revision))
        .collect();
    assert_eq!(
        summary,
        [
            ("Roadmap".to_owned(), true, 1),
            ("backlog/item-1".to_owned(), false, 2),
            ("notes".to_owned(), false, 3)
        ]
    );
    let at_two: Vec<String> = store
        .list_files("JD-1", Some(2))
        .unwrap()
        .into_iter()
        .map(|f| f.file)
        .collect();
    assert_eq!(at_two, ["Roadmap", "backlog/item-1"]);

    let main_doc = store.get("JD-1", None).unwrap();
    assert_eq!(main_doc.document.revision, 3);
    assert!(main_doc.document.multifile);
    assert_eq!(main_doc.document.files, 2);
    assert_eq!(main_doc.files, ["backlog/item-1", "notes"]);
    assert_eq!(main_doc.file, None);
    assert_eq!(main_doc.markdown.as_bytes(), main_bytes.as_slice());

    let got = store.get_file("JD-1", "Backlog/Item-1.md", None).unwrap();
    assert_eq!(got.file.as_deref(), Some("backlog/item-1"));
    assert_eq!(got.markdown, text);
    assert_eq!(got.revision.revision, 3);
    let old = store.get_file("JD-1", "backlog/item-1", Some(2)).unwrap();
    assert_eq!(old.markdown, text);
    assert_eq!(old.revision.revision, 2);
    let main_by_name = store.get_file("JD-1", "Roadmap", None).unwrap();
    assert_eq!(main_by_name.markdown.as_bytes(), main_bytes.as_slice());

    for (file, version) in [
        ("notes", Some(2)),
        ("backlog/item-1", Some(1)),
        ("missing", None),
    ] {
        let err = store.get_file("JD-1", file, version).unwrap_err();
        assert!(matches!(err, Error::FileNotFound { .. }), "{file}: {err}");
    }

    let listing = store
        .list(&joedocs::store::ListRequest {
            project: "joedocs".into(),
            doc_type: None,
            status: None,
            sort: joedocs::store::Sort::CreatedAsc,
            start: 1,
            limit: 10,
        })
        .unwrap();
    let counts: Vec<(bool, u64)> = listing
        .items
        .iter()
        .map(|d| (d.multifile, d.files))
        .collect();
    assert_eq!(counts, [(true, 2), (false, 0)]);
}

#[test]
fn file_commits_carry_their_files_in_trailers() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap(&mut store).unwrap();
    store
        .create_file(&actor(), &new_file("backlog/item-1", "x\n", None))
        .unwrap();
    let log = git(dir.path()).log().unwrap();
    let trailers = |index: usize| -> Vec<(String, String)> {
        log[index]
            .trailers
            .iter()
            .filter(|(k, _)| k != "JoeDocs-Client")
            .cloned()
            .collect()
    };
    let pair = |k: &str, v: &str| (k.to_owned(), v.to_owned());
    assert_eq!(
        trailers(0),
        [
            pair("JoeDocs-Document", "JD-1"),
            pair("JoeDocs-Revision", "1"),
            pair("JoeDocs-Action", "create"),
            pair("JoeDocs-File", "Roadmap.md"),
        ]
    );
    assert_eq!(
        trailers(1),
        [
            pair("JoeDocs-Document", "JD-2"),
            pair("JoeDocs-Revision", "1"),
            pair("JoeDocs-Action", "create"),
        ],
        "single-file commits are unchanged"
    );
    assert_eq!(
        trailers(2),
        [
            pair("JoeDocs-Document", "JD-1"),
            pair("JoeDocs-Revision", "2"),
            pair("JoeDocs-Action", "create_file"),
            pair("JoeDocs-File", "backlog/item-1.md"),
        ]
    );
    assert_eq!(log[2].subject, "Add a file to JD-1: backlog/item-1");
}

#[test]
fn bad_files_are_refused_and_change_nothing() {
    let dir = TempDir::new().unwrap();
    let mut settings = Settings::from_config(&Config::default());
    settings.max_files = 2;
    let mut store = open_with(dir.path(), settings).unwrap();
    roadmap(&mut store).unwrap();
    store
        .create_file(&actor(), &new_file("Backlog/Item", "x\n", None))
        .unwrap();
    let before = snapshot(dir.path()).unwrap();

    let mut on_single = new_file("x", "x\n", None);
    on_single.document_id = "JD-2".into();
    let mut bad_front_matter = new_file("y", "x\n", None);
    bad_front_matter.front_matter = fields(json!({"revision": 9}));
    let cases: Vec<(NewFile, &str)> = vec![
        (on_single, "joedocs document convert JD-2"),
        (new_file("../escape", "x\n", None), "invalid file"),
        (new_file("INDEX", "x\n", None), "reserved"),
        (new_file("roadmap", "x\n", None), "main file"),
        (new_file("backlog/item.md", "x\n", None), "already exists"),
        (new_file("y", "x\n", Some(9)), "revision 2, not 9"),
        (bad_front_matter, "owned by the server"),
    ];
    for (request, why) in cases {
        let err = store.create_file(&actor(), &request).unwrap_err();
        assert!(err.to_string().contains(why), "{why}: {err}");
    }
    fs::write(
        dir.path().join("vault/joedocs/plans/Roadmap/stray.md"),
        "not ours\n",
    )
    .unwrap();
    let err = store
        .create_file(&actor(), &new_file("stray", "x\n", None))
        .unwrap_err();
    assert!(err.to_string().contains("already exists"), "{err}");
    fs::remove_file(dir.path().join("vault/joedocs/plans/Roadmap/stray.md")).unwrap();
    assert_eq!(snapshot(dir.path()).unwrap(), before);

    store
        .create_file(&actor(), &new_file("second", "x\n", None))
        .unwrap();
    let err = store
        .create_file(&actor(), &new_file("third", "x\n", None))
        .unwrap_err();
    assert!(err.to_string().contains("documents.max_files"), "{err}");
}

#[test]
fn the_folder_index_lists_the_files_and_stays_out_of_the_history() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap(&mut store).unwrap();
    let index = dir.path().join("vault/joedocs/plans/Roadmap/INDEX.md");
    let first = fs::read_to_string(&index).unwrap();
    assert!(first.contains("Revision: 1"), "{first}");
    store
        .create_file(&actor(), &new_file("backlog/item-1", "x\n", None))
        .unwrap();
    let text = fs::read_to_string(&index).unwrap();
    assert_eq!(
        text,
        "# JD-1 Roadmap\n\n\
         Generated by JoeDocs after every change. Do not edit: changes are overwritten.\n\n\
         Revision: 2\n\n\
         | File | Revision | Updated |\n| --- | --- | --- |\n\
         | [[Roadmap/Roadmap\\|Roadmap]] | 1 | 2026-10-26 |\n\
         | [[Roadmap/backlog/item-1\\|backlog/item-1]] | 2 | 2026-10-26 |\n"
    );
    let status = Command::new("git")
        .arg("--git-dir")
        .arg(dir.path().join("state/history.git"))
        .arg("--work-tree")
        .arg(dir.path().join("vault"))
        .args(["status", "--porcelain"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&status.stdout), "");

    // Unchanged content is not rewritten; a deleted index comes back.
    let modified = fs::metadata(&index).unwrap().modified().unwrap();
    store.refresh_index_files();
    assert_eq!(fs::metadata(&index).unwrap().modified().unwrap(), modified);
    fs::remove_file(&index).unwrap();
    let report = store.check().unwrap();
    assert_eq!(report.stale_index_files, ["joedocs/plans/Roadmap/INDEX.md"]);
    store.refresh_index_files();
    assert_eq!(fs::read_to_string(&index).unwrap(), text);
    assert_eq!(store.check().unwrap().problems(), 0);
}

#[test]
fn without_index_files_no_folder_index_is_written() {
    let dir = TempDir::new().unwrap();
    let mut settings = Settings::from_config(&Config::default());
    settings.index_file = false;
    let mut store = open_with(dir.path(), settings).unwrap();
    roadmap(&mut store).unwrap();
    store
        .create_file(&actor(), &new_file("a", "x\n", None))
        .unwrap();
    assert!(
        !dir.path()
            .join("vault/joedocs/plans/Roadmap/INDEX.md")
            .exists()
    );
    assert_eq!(store.check().unwrap().problems(), 0);
}

#[test]
fn a_multi_file_document_named_index_gets_its_id_in_the_folder_name() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    store.add_project("joedocs", "JD", None).unwrap();
    let changed = store
        .create(
            &actor(),
            NewDocument {
                project: "joedocs".into(),
                doc_type: Some("notes".into()),
                title: "Index".into(),
                content: "x\n".into(),
                multifile: true,
                ..NewDocument::default()
            },
        )
        .unwrap();
    assert_eq!(changed.path, "joedocs/notes/Index (JD-1)/Index (JD-1).md");
    assert_eq!(store.check().unwrap().problems(), 0);
}

#[test]
fn reindex_rebuilds_files_identically_and_check_sees_their_drift() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap(&mut store).unwrap();
    let mut item = new_file("backlog/item-1", "item\n", None);
    item.front_matter = fields(json!({"status": "planned", "tags": ["x"]}));
    store.create_file(&actor(), &item).unwrap();
    store
        .create_file(&actor(), &new_file("notes", "notes\n", None))
        .unwrap();
    // Another document's file in between, so file IDs interleave.
    store
        .create(
            &actor(),
            NewDocument {
                project: "joedocs".into(),
                title: "Second".into(),
                content: "x\n".into(),
                multifile: true,
                ..NewDocument::default()
            },
        )
        .unwrap();
    let mut other = new_file("other", "x\n", None);
    other.document_id = "JD-3".into();
    store.create_file(&actor(), &other).unwrap();
    store
        .create_file(&actor(), &new_file("later", "later\n", None))
        .unwrap();
    assert_eq!(store.check().unwrap().problems(), 0);
    assert_rebuilds_identically(dir.path(), &mut store).unwrap();

    // A lost database comes back the same from the history alone.
    let before = snapshot(dir.path()).unwrap();
    drop(store);
    for suffix in ["", "-wal", "-shm"] {
        let _ = fs::remove_file(format!("{}{suffix}", paths(dir.path()).db().display()));
    }
    let mut store = open(dir.path()).unwrap();
    store.add_project("joedocs", "JD", Some("JoeDocs")).unwrap();
    store.reindex().unwrap();
    assert_eq!(snapshot(dir.path()).unwrap(), before);

    let item = dir
        .path()
        .join("vault/joedocs/plans/Roadmap/backlog/item-1.md");
    fs::write(&item, "edited by hand\n").unwrap();
    let stray = dir.path().join("vault/joedocs/plans/Roadmap/stray.md");
    fs::write(&stray, "x\n").unwrap();
    let report = store.check().unwrap();
    assert_eq!(
        report.drifted,
        [(
            "JD-1".to_owned(),
            "joedocs/plans/Roadmap/backlog/item-1.md".to_owned()
        )]
    );
    assert_eq!(report.unknown_files, ["joedocs/plans/Roadmap/stray.md"]);
    let err = store
        .create_file(&actor(), &new_file("more", "x\n", None))
        .unwrap_err();
    assert!(matches!(err, Error::Integrity { .. }), "{err}");
}
