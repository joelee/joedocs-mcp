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

// --- Changing files (STEP-06) -----------------------------------------------

/// JD-1 "Roadmap" (multi-file) at revision 3 with `backlog/item-1` (rev 2)
/// and `notes` (rev 3).
fn roadmap_with_files(store: &mut Store) -> TestResult<()> {
    roadmap(store)?;
    let mut item = new_file("backlog/item-1", "# Item 1\n", None);
    item.front_matter = fields(json!({"status": "planned"}));
    store.create_file(&actor(), &item)?;
    store.create_file(&actor(), &new_file("notes", "notes\n", None))?;
    Ok(())
}

fn read(root: &Path, path: &str) -> String {
    fs::read_to_string(root.join("vault").join(path)).unwrap_or_default()
}

#[test]
fn each_edit_tool_changes_one_file_in_one_commit() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap_with_files(&mut store).unwrap();
    let main_before = read(dir.path(), "joedocs/plans/Roadmap/Roadmap.md");
    let notes_before = read(dir.path(), "joedocs/plans/Roadmap/notes.md");
    let item = "joedocs/plans/Roadmap/backlog/item-1.md";

    let set = fields(json!({"status": "doing"}));
    let changed = store
        .update_file(
            &actor(),
            "JD-1",
            "backlog/item-1",
            "# Item 1 v2\n",
            &set,
            Some(3),
            None,
        )
        .unwrap();
    assert_eq!(
        (changed.revision, changed.file.as_deref()),
        (4, Some("backlog/item-1"))
    );
    assert_eq!(changed.front_matter["status"], json!("doing"));
    assert_eq!(changed.front_matter["revision"], json!(4));
    let edits = [joedocs::store::Replacement {
        old: "v2".into(),
        new: "v3".into(),
    }];
    store
        .edit_file(&actor(), "JD-1", "backlog/item-1", &edits, Some(4), None)
        .unwrap();
    store
        .append_file(&actor(), "JD-1", "backlog/item-1", "more\n", None, None)
        .unwrap();
    let set = fields(json!({"owner": "joel"}));
    store
        .change_file_front_matter(
            &actor(),
            "JD-1",
            "backlog/item-1",
            &set,
            &["status".into()],
            None,
        )
        .unwrap();
    let text = read(dir.path(), item);
    assert!(
        text.starts_with("---\ndocument: JD-1\nrevision: 7\n"),
        "{text}"
    );
    assert!(
        text.ends_with("owner: joel\n---\n# Item 1 v3\n\nmore\n"),
        "{text}"
    );
    assert!(!text.contains("status:"), "{text}");
    assert_eq!(
        read(dir.path(), "joedocs/plans/Roadmap/Roadmap.md"),
        main_before
    );
    assert_eq!(
        read(dir.path(), "joedocs/plans/Roadmap/notes.md"),
        notes_before
    );

    let actions: Vec<(String, String)> = git(dir.path())
        .log()
        .unwrap()
        .iter()
        .skip(4)
        .map(|c| {
            let files: Vec<&str> = c
                .trailers
                .iter()
                .filter(|(k, _)| k == "JoeDocs-File")
                .map(|(_, v)| v.as_str())
                .collect();
            (
                c.trailer("JoeDocs-Action").unwrap_or("").to_owned(),
                files.join(","),
            )
        })
        .collect();
    assert_eq!(
        actions,
        [
            ("update".to_owned(), "backlog/item-1.md".to_owned()),
            ("edit".to_owned(), "backlog/item-1.md".to_owned()),
            ("append".to_owned(), "backlog/item-1.md".to_owned()),
            ("front_matter".to_owned(), "backlog/item-1.md".to_owned()),
        ]
    );

    assert_rebuilds_identically(dir.path(), &mut store).unwrap();
}

#[test]
fn file_changes_that_change_nothing_or_miss_the_file_are_refused() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap_with_files(&mut store).unwrap();
    let before = snapshot(dir.path()).unwrap();
    let same = fields(json!({"status": "planned"}));
    let no_op = [joedocs::store::Replacement {
        old: "Item".into(),
        new: "Item".into(),
    }];
    let id = "JD-1";
    let item = "backlog/item-1";
    let refusals = [
        store
            .update_file(
                &actor(),
                id,
                item,
                "# Item 1\n",
                &Fields::new(),
                Some(3),
                None,
            )
            .unwrap_err(),
        store
            .change_file_front_matter(&actor(), id, item, &same, &[], None)
            .unwrap_err(),
        store
            .change_file_front_matter(&actor(), id, item, &Fields::new(), &[], None)
            .unwrap_err(),
        store
            .edit_file(&actor(), id, item, &no_op, None, None)
            .unwrap_err(),
        store
            .append_file(&actor(), id, item, " \n", None, None)
            .unwrap_err(),
    ];
    for err in refusals {
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
    }
    let err = store
        .update_file(&actor(), id, "missing", "x\n", &Fields::new(), None, None)
        .unwrap_err();
    assert!(matches!(err, Error::FileNotFound { .. }), "{err}");
    let err = store
        .append_file(&actor(), "JD-2", "x", "x\n", None, None)
        .unwrap_err();
    assert!(err.to_string().contains("single-file document"), "{err}");
    assert_eq!(snapshot(dir.path()).unwrap(), before);
}

#[test]
fn a_stale_revision_conflicts_only_for_the_file_that_changed() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap_with_files(&mut store).unwrap();
    // Two agents read revision 3 and change different files: both succeed.
    store
        .append_file(&actor(), "JD-1", "backlog/item-1", "a\n", Some(3), None)
        .unwrap();
    let second = store
        .append_file(&actor(), "JD-1", "notes", "b\n", Some(3), None)
        .unwrap();
    assert_eq!(second.revision, 5);
    // The main file last changed at 1, so revision 3 is not stale for it.
    let main = store
        .append(&actor(), "JD-1", "c\n", Some(3), None)
        .unwrap();
    assert_eq!(main.revision, 6);

    let stale = store
        .append_file(&actor(), "JD-1", "backlog/item-1", "again\n", Some(3), None)
        .unwrap_err();
    match &stale {
        Error::FileConflict {
            file,
            file_revision,
            current,
            ..
        } => assert_eq!(
            (file.as_str(), *file_revision, *current),
            ("backlog/item-1", 4, 6)
        ),
        other => panic!("{other}"),
    }
    let stale_main = store
        .append(&actor(), "JD-1", "d\n", Some(5), None)
        .unwrap_err();
    assert!(
        matches!(
            stale_main,
            Error::FileConflict {
                file_revision: 6,
                ..
            }
        ),
        "{stale_main}"
    );
    let ahead = store
        .append_file(&actor(), "JD-1", "notes", "x\n", Some(9), None)
        .unwrap_err();
    assert!(matches!(ahead, Error::FileConflict { .. }), "{ahead}");
    // Whole-document operations still need the current revision.
    let rename = store
        .rename(&actor(), "JD-2", "Plan B", Some(9))
        .unwrap_err();
    assert!(matches!(rename, Error::Conflict { .. }), "{rename}");
    assert_rebuilds_identically(dir.path(), &mut store).unwrap();
}

#[test]
fn files_are_renamed_within_the_document_and_keep_their_history() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap_with_files(&mut store).unwrap();
    let changed = store
        .rename_file(&actor(), "JD-1", "notes", "archive/2026/Notes.md", Some(3))
        .unwrap();
    assert_eq!(
        (
            changed.revision,
            changed.path.as_str(),
            changed.file.as_deref()
        ),
        (
            4,
            "joedocs/plans/Roadmap/archive/2026/Notes.md",
            Some("archive/2026/Notes")
        )
    );
    assert!(
        !dir.path()
            .join("vault/joedocs/plans/Roadmap/notes.md")
            .exists()
    );
    let text = read(dir.path(), "joedocs/plans/Roadmap/archive/2026/Notes.md");
    assert!(
        text.starts_with("---\ndocument: JD-1\nrevision: 4\n"),
        "{text}"
    );
    let last = git(dir.path()).log().unwrap().pop().unwrap();
    let files: Vec<(String, String)> = last
        .trailers
        .into_iter()
        .filter(|(k, _)| k.starts_with("JoeDocs-File"))
        .collect();
    assert_eq!(
        files,
        [
            (
                "JoeDocs-File".to_owned(),
                "archive/2026/Notes.md".to_owned()
            ),
            ("JoeDocs-File-From".to_owned(), "notes.md".to_owned()),
        ]
    );
    // Back to the top level, then a case-only rename.
    store
        .rename_file(&actor(), "JD-1", "archive/2026/notes", "Notes", None)
        .unwrap();
    assert!(
        !dir.path()
            .join("vault/joedocs/plans/Roadmap/archive")
            .exists()
    );
    store
        .rename_file(&actor(), "JD-1", "Notes", "notes", None)
        .unwrap();
    assert!(read(dir.path(), "joedocs/plans/Roadmap/notes.md").contains("revision: 6"));
    let names: Vec<String> = store
        .list_files("JD-1", Some(4))
        .unwrap()
        .into_iter()
        .map(|f| f.file)
        .collect();
    assert_eq!(names, ["Roadmap", "archive/2026/Notes", "backlog/item-1"]);
    let old = store
        .get_file("JD-1", "archive/2026/Notes", Some(4))
        .unwrap();
    assert!(old.markdown.contains("revision: 4"));

    for (from, to, why) in [
        ("notes", "notes", "already the file's path"),
        ("notes", "backlog/item-1", "already exists"),
        ("notes", "Roadmap", "main file"),
        ("notes", "../x", "`..`"),
    ] {
        let err = store
            .rename_file(&actor(), "JD-1", from, to, None)
            .unwrap_err();
        assert!(err.to_string().contains(why), "{why}: {err}");
    }
    assert_rebuilds_identically(dir.path(), &mut store).unwrap();
}

fn write(file: &str, content: &str, front_matter: serde_json::Value) -> joedocs::store::FileWrite {
    joedocs::store::FileWrite {
        file: file.into(),
        content: content.into(),
        front_matter: fields(front_matter),
    }
}

#[test]
fn several_files_are_written_in_one_revision_or_not_at_all() {
    let dir = TempDir::new().unwrap();
    let mut settings = Settings::from_config(&Config::default());
    settings.max_files = 4;
    let mut store = open_with(dir.path(), settings).unwrap();
    roadmap_with_files(&mut store).unwrap();
    let commits = git(dir.path()).log().unwrap().len();

    let written = store
        .write_files(
            &actor(),
            &joedocs::store::WriteFiles {
                document_id: "JD-1".into(),
                files: vec![
                    write("backlog/item-1", "# Item 1 v2\n", json!({"status": "done"})),
                    write("backlog/item-2", "# Item 2\n", json!({"status": "planned"})),
                    // Identical: not counted as a change.
                    write("notes", "notes\n", json!({})),
                ],
                expected_revision: Some(3),
                summary: Some("triage".into()),
            },
        )
        .unwrap();
    assert_eq!(written.revision, 4);
    let names: Vec<(&str, bool)> = written
        .files
        .iter()
        .map(|f| (f.file.as_str(), f.created))
        .collect();
    assert_eq!(names, [("backlog/item-1", false), ("backlog/item-2", true)]);
    let log = git(dir.path()).log().unwrap();
    assert_eq!(log.len(), commits + 1);
    let last = log.last().unwrap();
    assert_eq!(last.trailer("JoeDocs-Action"), Some("write_files"));
    assert_eq!(last.subject, "Write files of JD-1: triage");
    assert!(read(dir.path(), "joedocs/plans/Roadmap/notes.md").contains("revision: 3"));

    let before = snapshot(dir.path()).unwrap();
    let request =
        |files: Vec<joedocs::store::FileWrite>, expected: Option<u64>| joedocs::store::WriteFiles {
            document_id: "JD-1".into(),
            files,
            expected_revision: expected,
            summary: None,
        };
    for (files, expected, why) in [
        (
            vec![
                write("a", "x\n", json!({})),
                write("../b", "x\n", json!({})),
            ],
            None,
            "file 2",
        ),
        (
            vec![
                write("a", "x\n", json!({})),
                write("A.md", "y\n", json!({})),
            ],
            None,
            "twice",
        ),
        (
            vec![write("a", "x\n", json!({})), write("b", "y\n", json!({}))],
            None,
            "documents.max_files",
        ),
        (
            vec![write("backlog/item-1", "z\n", json!({}))],
            Some(3),
            "changed at revision 4",
        ),
        (
            vec![write("notes", "notes\n", json!({}))],
            None,
            "nothing would change",
        ),
        (vec![write("Roadmap", "x\n", json!({}))], None, "main file"),
        (
            vec![write("a", "x\n", json!({"revision": 1}))],
            None,
            "owned by the server",
        ),
        (vec![], None, "1 to 4"),
    ] {
        let err = store
            .write_files(&actor(), &request(files, expected))
            .unwrap_err();
        assert!(err.to_string().contains(why), "{why}: {err}");
    }
    assert!(!dir.path().join("vault/joedocs/plans/Roadmap/a.md").exists());
    assert_eq!(snapshot(dir.path()).unwrap(), before);
    assert_rebuilds_identically(dir.path(), &mut store).unwrap();
}

// --- Whole-document operations on folders (STEP-07) -------------------------

fn vault_tree(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
            if name.starts_with('.') || name.contains("/.") {
                continue;
            }
            if entry.path().is_dir() {
                out.push(format!("{name}/"));
                walk(&entry.path(), &format!("{name}/"), out);
            } else {
                out.push(name);
            }
        }
    }
    let mut out = Vec::new();
    walk(&root.join("vault"), "", &mut out);
    out.sort();
    out
}

#[test]
fn renaming_and_moving_a_multi_file_document_moves_its_folder() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap_with_files(&mut store).unwrap();
    let renamed = store.rename(&actor(), "JD-1", "Road map", Some(3)).unwrap();
    assert_eq!(renamed.path, "joedocs/plans/Road map/Road map.md");
    assert_eq!(renamed.front_matter["aliases"], json!(["Roadmap"]));
    let tree = vault_tree(dir.path());
    assert!(
        !tree.iter().any(|p| p.starts_with("joedocs/plans/Roadmap")),
        "{tree:?}"
    );
    for file in ["Road map.md", "INDEX.md", "backlog/item-1.md", "notes.md"] {
        assert!(
            tree.contains(&format!("joedocs/plans/Road map/{file}")),
            "{file}: {tree:?}"
        );
    }
    let item = read(dir.path(), "joedocs/plans/Road map/backlog/item-1.md");
    assert!(
        item.contains("revision: 2"),
        "sub-files are moved, not changed: {item}"
    );
    let last = git(dir.path()).log().unwrap().pop().unwrap();
    let files: Vec<(String, String)> = last
        .trailers
        .iter()
        .filter(|(k, _)| k.starts_with("JoeDocs-File"))
        .cloned()
        .collect();
    assert_eq!(
        files,
        [
            ("JoeDocs-File".to_owned(), "Road map.md".to_owned()),
            ("JoeDocs-File-From".to_owned(), "Roadmap.md".to_owned()),
        ]
    );
    // Files at an older revision are still found under their old folder.
    let old = store.get_file("JD-1", "backlog/item-1", Some(3)).unwrap();
    assert!(old.markdown.contains("# Item 1"));
    assert_eq!(store.check().unwrap().problems(), 0);

    let moved = store
        .move_document(&actor(), "JD-1", "specs", Some(4))
        .unwrap();
    assert_eq!(moved.path, "joedocs/specs/Road map/Road map.md");
    assert!(!dir.path().join("vault/joedocs/plans/Road map").exists());
    assert!(read(dir.path(), "joedocs/specs/Road map/notes.md").contains("notes"));
    // A case-only rename of the folder.
    store.rename(&actor(), "JD-1", "Road Map", Some(5)).unwrap();
    assert!(read(dir.path(), "joedocs/specs/Road Map/notes.md").contains("notes"));
    assert_eq!(store.check().unwrap().problems(), 0);
    assert_rebuilds_identically(dir.path(), &mut store).unwrap();
}

#[test]
fn archiving_a_multi_file_document_removes_and_restores_its_folder() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap_with_files(&mut store).unwrap();
    let archived = store.archive(&actor(), "JD-1", "done").unwrap();
    assert_eq!(archived.revision, 4);
    assert!(!dir.path().join("vault/joedocs/plans/Roadmap").exists());
    assert_eq!(store.check().unwrap().problems(), 0);
    assert_rebuilds_identically(dir.path(), &mut store).unwrap();

    let back = store.unarchive(&actor(), "JD-1").unwrap();
    assert_eq!(back.revision, 5);
    assert_eq!(back.path, "joedocs/plans/Roadmap/Roadmap.md");
    let names: Vec<(String, u64)> = store
        .list_files("JD-1", None)
        .unwrap()
        .into_iter()
        .map(|f| (f.file, f.revision))
        .collect();
    assert_eq!(
        names,
        [
            ("Roadmap".to_owned(), 5),
            ("backlog/item-1".to_owned(), 5),
            ("notes".to_owned(), 5)
        ]
    );
    let item = read(dir.path(), "joedocs/plans/Roadmap/backlog/item-1.md");
    assert!(
        item.contains("revision: 5") && item.contains("status: planned"),
        "{item}"
    );
    assert!(item.ends_with("# Item 1\n"), "{item}");
    assert_eq!(store.check().unwrap().problems(), 0);
    assert_rebuilds_identically(dir.path(), &mut store).unwrap();
}

#[test]
fn one_file_is_archived_and_its_history_stays() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap_with_files(&mut store).unwrap();
    let archived = store
        .archive_file(&actor(), "JD-1", "backlog/item-1", Some(3), Some("shipped"))
        .unwrap();
    assert_eq!(
        (archived.revision, archived.file.as_str()),
        (4, "backlog/item-1")
    );
    assert!(
        !dir.path()
            .join("vault/joedocs/plans/Roadmap/backlog")
            .exists()
    );
    let names: Vec<String> = store
        .list_files("JD-1", None)
        .unwrap()
        .into_iter()
        .map(|f| f.file)
        .collect();
    assert_eq!(names, ["Roadmap", "notes"]);
    let old = store.get_file("JD-1", "backlog/item-1", Some(3)).unwrap();
    assert!(old.markdown.contains("# Item 1"));
    let last = git(dir.path()).log().unwrap().pop().unwrap();
    assert_eq!(last.trailer("JoeDocs-Action"), Some("archive_file"));
    assert_eq!(
        last.subject,
        "Archive a file of JD-1: backlog/item-1: shipped"
    );
    // The path is free again.
    store
        .create_file(&actor(), &new_file("backlog/item-1", "again\n", None))
        .unwrap();
    for (file, why) in [("Roadmap", "main file"), ("missing", "has no file")] {
        let err = store
            .archive_file(&actor(), "JD-1", file, None, None)
            .unwrap_err();
        assert!(err.to_string().contains(why), "{why}: {err}");
    }
    let stale = store
        .archive_file(&actor(), "JD-1", "backlog/item-1", Some(4), None)
        .unwrap_err();
    assert!(matches!(stale, Error::FileConflict { .. }), "{stale}");
    assert_rebuilds_identically(dir.path(), &mut store).unwrap();
}

#[test]
fn restoring_a_folder_brings_back_its_files_as_they_were() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap_with_files(&mut store).unwrap();
    // After revision 3: change one file, archive one, rename one, add one.
    store
        .append_file(&actor(), "JD-1", "notes", "later\n", None, None)
        .unwrap();
    store
        .archive_file(&actor(), "JD-1", "backlog/item-1", None, None)
        .unwrap();
    store
        .create_file(&actor(), &new_file("new", "new\n", None))
        .unwrap();
    store
        .append(&actor(), "JD-1", "main later\n", None, None)
        .unwrap();
    store
        .rename_file(&actor(), "JD-1", "notes", "renamed", None)
        .unwrap();
    let main_before = read(dir.path(), "joedocs/plans/Roadmap/Roadmap.md");

    let restored = store
        .restore_version(&actor(), "JD-1", 3, Some(8), None)
        .unwrap();
    assert_eq!(restored.revision, 9);
    let names: Vec<String> = store
        .list_files("JD-1", None)
        .unwrap()
        .into_iter()
        .map(|f| f.file)
        .collect();
    assert_eq!(names, ["Roadmap", "backlog/item-1", "notes"]);
    let notes = read(dir.path(), "joedocs/plans/Roadmap/notes.md");
    assert!(notes.ends_with("---\nnotes\n"), "{notes}");
    assert!(notes.contains("revision: 9"), "{notes}");
    assert!(
        read(dir.path(), "joedocs/plans/Roadmap/backlog/item-1.md").contains("status: planned")
    );
    assert!(
        !dir.path()
            .join("vault/joedocs/plans/Roadmap/new.md")
            .exists()
    );
    assert!(
        !dir.path()
            .join("vault/joedocs/plans/Roadmap/renamed.md")
            .exists()
    );
    let main = read(dir.path(), "joedocs/plans/Roadmap/Roadmap.md");
    assert_ne!(main, main_before);
    assert!(main.ends_with("---\n# Roadmap\n"), "{main}");
    let stale = store
        .restore_version(&actor(), "JD-1", 2, Some(8), None)
        .unwrap_err();
    assert!(matches!(stale, Error::Conflict { .. }), "{stale}");
    assert_eq!(store.check().unwrap().problems(), 0);
    assert_rebuilds_identically(dir.path(), &mut store).unwrap();
}

#[test]
fn a_file_edited_by_hand_blocks_every_folder_operation() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap_with_files(&mut store).unwrap();
    let item = dir
        .path()
        .join("vault/joedocs/plans/Roadmap/backlog/item-1.md");
    fs::write(&item, "edited by hand\n").unwrap();
    let tree = vault_tree(dir.path());
    let before = snapshot(dir.path()).unwrap();
    let errors = [
        store.rename(&actor(), "JD-1", "Other", None).unwrap_err(),
        store
            .move_document(&actor(), "JD-1", "specs", None)
            .unwrap_err(),
        store.archive(&actor(), "JD-1", "x").unwrap_err(),
        store
            .restore_version(&actor(), "JD-1", 2, None, None)
            .unwrap_err(),
        store
            .archive_file(&actor(), "JD-1", "notes", None, None)
            .unwrap_err(),
        store
            .append(&actor(), "JD-1", "x\n", None, None)
            .unwrap_err(),
    ];
    for err in errors {
        assert!(matches!(err, Error::Integrity { .. }), "{err}");
    }
    assert_eq!(vault_tree(dir.path()), tree);
    assert_eq!(snapshot(dir.path()).unwrap(), before);
    assert_eq!(fs::read_to_string(&item).unwrap(), "edited by hand\n");
}

// --- History and diff by file (STEP-08) -------------------------------------

/// JD-1 after: 4 append notes, 5 rename notes → n2, 6 append main,
/// 7 append n2.
fn roadmap_with_history(store: &mut Store) -> TestResult<()> {
    roadmap_with_files(store)?;
    store.append_file(&actor(), "JD-1", "notes", "later\n", None, None)?;
    store.rename_file(&actor(), "JD-1", "notes", "n2", None)?;
    store.append(&actor(), "JD-1", "main later\n", None, None)?;
    store.append_file(&actor(), "JD-1", "n2", "last\n", None, None)?;
    Ok(())
}

#[test]
fn a_files_history_follows_its_renames() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap_with_history(&mut store).unwrap();
    let page = store.history_file("JD-1", "n2", 1, 50).unwrap();
    let entries: Vec<(u64, Option<String>, String)> = page
        .revisions
        .iter()
        .map(|r| (r.revision, r.file.clone(), r.action.clone()))
        .collect();
    let entry = |revision: u64, file: &str, action: &str| {
        (revision, Some(file.to_owned()), action.to_owned())
    };
    assert_eq!(
        entries,
        [
            entry(7, "n2", "append"),
            entry(5, "n2", "rename"),
            entry(4, "notes", "append"),
            entry(3, "notes", "create_file"),
        ]
    );
    assert_eq!((page.total, page.next_start), (4, None));
    assert_eq!(page.revisions[2].path, "joedocs/plans/Roadmap/notes.md");
    let paged = store.history_file("JD-1", "n2", 2, 2).unwrap();
    assert_eq!(
        paged
            .revisions
            .iter()
            .map(|r| r.revision)
            .collect::<Vec<_>>(),
        [5, 4]
    );
    assert_eq!(paged.next_start, Some(4));
    let main: Vec<u64> = store
        .history_file("JD-1", "Roadmap", 1, 50)
        .unwrap()
        .revisions
        .iter()
        .map(|r| r.revision)
        .collect();
    assert_eq!(main, [6, 1]);

    // An archived file's history is still there.
    store
        .archive_file(&actor(), "JD-1", "backlog/item-1", None, None)
        .unwrap();
    let gone: Vec<u64> = store
        .history_file("JD-1", "backlog/item-1", 1, 50)
        .unwrap()
        .revisions
        .iter()
        .map(|r| r.revision)
        .collect();
    assert_eq!(gone, [8, 2]);
    let err = store.history_file("JD-1", "never", 1, 50).unwrap_err();
    assert!(matches!(err, Error::FileNotFound { .. }), "{err}");
    // Whole-document history is unchanged, with no file on its entries.
    let whole = store.history("JD-1", 1, 50).unwrap();
    assert_eq!(whole.total, 8);
    assert!(whole.revisions.iter().all(|r| r.file.is_none()));
}

#[test]
fn diffs_cover_one_file_or_every_file_that_changed() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap_with_history(&mut store).unwrap();
    let one = store.diff_file("JD-1", "n2", 3, Some(7), 0).unwrap();
    assert_eq!(one.files, ["n2"]);
    assert!(
        one.diff
            .starts_with("--- JD-1 revision 3: notes\n+++ JD-1 revision 7: n2\n"),
        "{}",
        one.diff
    );
    assert!(
        one.diff.contains("+later\n") && one.diff.contains("+last\n"),
        "{}",
        one.diff
    );
    assert!(
        one.diff.contains("-revision: 3\n+revision: 7\n"),
        "{}",
        one.diff
    );
    let none = store
        .diff_file("JD-1", "backlog/item-1", 2, Some(7), 3)
        .unwrap();
    assert_eq!(none.diff, "");
    let before = store.diff_file("JD-1", "n2", 2, None, 3).unwrap_err();
    assert!(before.to_string().contains("revision 2"), "{before}");

    let all = store.diff("JD-1", 3, Some(7), 0).unwrap();
    assert_eq!(all.files, ["Roadmap", "n2", "notes"]);
    let text = &all.diff;
    assert!(
        text.contains("--- JD-1 revision 3: Roadmap\n+++ JD-1 revision 7: Roadmap\n"),
        "{text}"
    );
    assert!(text.contains("+main later\n"), "{text}");
    assert!(
        text.contains(
            "--- JD-1 revision 3: notes\n+++ JD-1 revision 7: notes (removed)\n@@ -1,6 +0,0 @@\n"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "--- JD-1 revision 3: n2 (added)\n+++ JD-1 revision 7: n2\n@@ -0,0 +1,10 @@\n"
        ),
        "{text}"
    );
    assert!(!text.contains("item-1"), "{text}");
    assert_eq!(store.diff("JD-1", 7, None, 3).unwrap().diff, "");

    // Single-file diffs keep their form.
    store
        .append(&actor(), "JD-2", "more\n", None, None)
        .unwrap();
    let single = store.diff("JD-2", 1, None, 0).unwrap();
    assert!(
        single
            .diff
            .starts_with("--- JD-2 revision 1\n+++ JD-2 revision 2\n"),
        "{}",
        single.diff
    );
    assert_eq!(single.files, Vec::<String>::new());
}

// --- Search over sub-files (STEP-09) ----------------------------------------

fn search(
    store: &Store,
    query: &str,
    filters: serde_json::Value,
    document_id: Option<&str>,
) -> Vec<(String, Option<String>)> {
    let serde_json::Value::Object(filters) = filters else {
        return Vec::new();
    };
    store
        .search(&joedocs::store::SearchRequest {
            projects: vec!["joedocs".into()],
            doc_type: None,
            query: query.into(),
            filters,
            start: 1,
            limit: 50,
            document_id: document_id.map(str::to_owned),
        })
        .map(|results| {
            results
                .items
                .into_iter()
                .map(|hit| (hit.document.id, hit.file))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn search_finds_sub_files_with_their_own_front_matter() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap_with_files(&mut store).unwrap();
    let hit = |id: &str, file: Option<&str>| (id.to_owned(), file.map(str::to_owned));

    assert_eq!(
        search(&store, "item", json!({}), None),
        [hit("JD-1", Some("backlog/item-1"))]
    );
    let results = store
        .search(&joedocs::store::SearchRequest {
            projects: vec!["joedocs".into()],
            doc_type: None,
            query: "item".into(),
            filters: serde_json::Map::new(),
            start: 1,
            limit: 10,
            document_id: None,
        })
        .unwrap();
    assert!(
        results.items[0]
            .snippet
            .as_deref()
            .unwrap_or("")
            .contains("«Item»")
    );
    assert_eq!(
        search(&store, "", json!({"status": "planned"}), None),
        [hit("JD-1", Some("backlog/item-1"))]
    );
    let mut drafts = search(&store, "", json!({"status": "draft"}), None);
    drafts.sort();
    assert_eq!(drafts, [hit("JD-1", None), hit("JD-2", None)]);
    let mut everything = search(&store, "", json!({}), Some("JD-1"));
    everything.sort();
    assert_eq!(
        everything,
        [
            hit("JD-1", None),
            hit("JD-1", Some("backlog/item-1")),
            hit("JD-1", Some("notes"))
        ]
    );
    assert_eq!(search(&store, "plan", json!({}), Some("JD-1")), []);
    assert_eq!(search(&store, "plan", json!({}), None), [hit("JD-2", None)]);

    let before = (
        search(&store, "notes", json!({}), None),
        search(&store, "", json!({"status": "planned"}), None),
    );
    store.reindex().unwrap();
    assert_eq!(
        (
            search(&store, "notes", json!({}), None),
            search(&store, "", json!({"status": "planned"}), None)
        ),
        before
    );

    store
        .archive_file(&actor(), "JD-1", "backlog/item-1", None, None)
        .unwrap();
    assert_eq!(search(&store, "item", json!({}), None), []);
    assert_eq!(search(&store, "", json!({"status": "planned"}), None), []);
    store.archive(&actor(), "JD-1", "done").unwrap();
    assert_eq!(search(&store, "notes", json!({}), None), []);
}

// --- Admin-only operations (STEP-10) -----------------------------------------

#[test]
fn a_single_file_document_converts_to_a_folder_keeping_its_history() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap(&mut store).unwrap();
    store
        .append(&actor(), "JD-2", "more\n", None, None)
        .unwrap();
    let converted = store.convert(&actor(), "JD-2").unwrap();
    assert_eq!(
        (converted.revision, converted.path.as_str()),
        (3, "joedocs/plans/Plan/Plan.md")
    );
    assert!(!dir.path().join("vault/joedocs/plans/Plan.md").exists());
    let got = store.get("JD-2", None).unwrap();
    assert!(got.document.multifile);
    assert!(got.markdown.ends_with("plan\n\nmore\n"), "{}", got.markdown);
    assert!(
        store
            .get("JD-2", Some(1))
            .unwrap()
            .markdown
            .ends_with("---\nplan\n")
    );
    let mut file = new_file("notes", "x\n", None);
    file.document_id = "JD-2".into();
    assert_eq!(store.create_file(&actor(), &file).unwrap().revision, 4);
    let last_convert = git(dir.path())
        .log()
        .unwrap()
        .into_iter()
        .find(|c| c.trailer("JoeDocs-Action") == Some("convert"))
        .unwrap();
    assert_eq!(last_convert.subject, "Convert JD-2: single file → folder");
    let follow = Command::new("git")
        .arg("--git-dir")
        .arg(dir.path().join("state/history.git"))
        .args([
            "log",
            "--follow",
            "--format=%s",
            "--",
            "joedocs/plans/Plan/Plan.md",
        ])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    let subjects = String::from_utf8_lossy(&follow.stdout);
    assert!(subjects.contains("Create JD-2"), "{subjects}");
    let again = store.convert(&actor(), "JD-1").unwrap_err();
    assert!(
        again.to_string().contains("already a multi-file document"),
        "{again}"
    );
    assert_eq!(store.check().unwrap().problems(), 0);
    assert_rebuilds_identically(dir.path(), &mut store).unwrap();
}

/// JD-3, a multi-file "Backlog" with the file `item-2`.
fn backlog(store: &mut Store) -> TestResult<()> {
    store.create(
        &actor(),
        NewDocument {
            project: "joedocs".into(),
            doc_type: Some("plans".into()),
            title: "Backlog".into(),
            content: "# Backlog\n".into(),
            multifile: true,
            ..NewDocument::default()
        },
    )?;
    let mut file = new_file("item-2", "two\n", None);
    file.document_id = "JD-3".into();
    store.create_file(&actor(), &file)?;
    Ok(())
}

#[test]
fn a_file_moves_to_another_document_in_one_commit() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap_with_files(&mut store).unwrap();
    backlog(&mut store).unwrap();
    let commits = git(dir.path()).log().unwrap().len();
    let moved = store
        .move_file(&actor(), "JD-1", "backlog/item-1", "JD-3", Some("item-1"))
        .unwrap();
    assert_eq!(
        (
            moved.source_revision,
            moved.target_revision,
            moved.file.as_str()
        ),
        (4, 3, "item-1")
    );
    assert_eq!(moved.path, "joedocs/plans/Backlog/item-1.md");
    let log = git(dir.path()).log().unwrap();
    assert_eq!(log.len(), commits + 1);
    let last = log.last().unwrap();
    let trailers: Vec<(String, String)> = last
        .trailers
        .iter()
        .filter(|(k, _)| k != "JoeDocs-Client")
        .cloned()
        .collect();
    let pair = |k: &str, v: &str| (k.to_owned(), v.to_owned());
    assert_eq!(
        trailers,
        [
            pair("JoeDocs-Document", "JD-1"),
            pair("JoeDocs-Revision", "4"),
            pair("JoeDocs-Action", "move_file"),
            pair("JoeDocs-File", "backlog/item-1.md"),
            pair("JoeDocs-Target-Document", "JD-3"),
            pair("JoeDocs-Target-Revision", "3"),
            pair("JoeDocs-Target-File", "item-1.md"),
        ]
    );
    let text = read(dir.path(), "joedocs/plans/Backlog/item-1.md");
    assert!(
        text.starts_with("---\ndocument: JD-3\nrevision: 3\n"),
        "{text}"
    );
    assert!(text.contains("status: planned"), "{text}");
    assert!(
        !dir.path()
            .join("vault/joedocs/plans/Roadmap/backlog")
            .exists()
    );
    let names = |store: &Store, id: &str| -> Vec<String> {
        store
            .list_files(id, None)
            .unwrap()
            .into_iter()
            .map(|f| f.file)
            .collect()
    };
    assert_eq!(names(&store, "JD-1"), ["Roadmap", "notes"]);
    assert_eq!(names(&store, "JD-3"), ["Backlog", "item-1", "item-2"]);

    for (id, file, to, why) in [
        ("JD-1", "notes", "JD-2", "single-file"),
        ("JD-3", "item-2", "JD-3", "same document"),
        ("JD-1", "Roadmap", "JD-3", "main file"),
        ("JD-1", "missing", "JD-3", "has no file"),
    ] {
        let err = store.move_file(&actor(), id, file, to, None).unwrap_err();
        assert!(err.to_string().contains(why), "{why}: {err}");
    }
    let mut taken = new_file("notes", "x\n", None);
    taken.document_id = "JD-3".into();
    store.create_file(&actor(), &taken).unwrap();
    let err = store
        .move_file(&actor(), "JD-1", "notes", "JD-3", None)
        .unwrap_err();
    assert!(err.to_string().contains("already exists"), "{err}");
    assert_eq!(store.check().unwrap().problems(), 0);
    assert_rebuilds_identically(dir.path(), &mut store).unwrap();
}

#[test]
fn one_file_is_restored_as_it_was_even_after_archiving_or_renaming() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap_with_files(&mut store).unwrap();
    store
        .append_file(&actor(), "JD-1", "notes", "later\n", None, None)
        .unwrap();
    let restored = store.restore_file(&actor(), "JD-1", "notes", 3).unwrap();
    assert_eq!(restored.revision, 5);
    let notes = read(dir.path(), "joedocs/plans/Roadmap/notes.md");
    assert!(
        notes.contains("revision: 5") && notes.ends_with("---\nnotes\n"),
        "{notes}"
    );
    let unchanged = store
        .restore_file(&actor(), "JD-1", "notes", 3)
        .unwrap_err();
    assert!(
        unchanged.to_string().contains("already as it was"),
        "{unchanged}"
    );

    store
        .archive_file(&actor(), "JD-1", "backlog/item-1", None, None)
        .unwrap();
    let back = store
        .restore_file(&actor(), "JD-1", "backlog/item-1", 2)
        .unwrap();
    assert_eq!(back.revision, 7);
    assert!(
        read(dir.path(), "joedocs/plans/Roadmap/backlog/item-1.md").contains("status: planned")
    );

    store
        .rename_file(&actor(), "JD-1", "notes", "n2", None)
        .unwrap();
    store
        .append_file(&actor(), "JD-1", "n2", "more\n", None, None)
        .unwrap();
    let renamed_back = store.restore_file(&actor(), "JD-1", "notes", 4).unwrap();
    assert_eq!(renamed_back.file.as_deref(), Some("notes"));
    assert!(
        !dir.path()
            .join("vault/joedocs/plans/Roadmap/n2.md")
            .exists()
    );
    assert!(read(dir.path(), "joedocs/plans/Roadmap/notes.md").ends_with("notes\n\nlater\n"));

    store
        .create_file(&actor(), &new_file("e", "first\n", None))
        .unwrap();
    store
        .archive_file(&actor(), "JD-1", "e", None, None)
        .unwrap();
    store
        .create_file(&actor(), &new_file("e", "second\n", None))
        .unwrap();
    let taken = store.restore_file(&actor(), "JD-1", "e", 11).unwrap_err();
    assert!(taken.to_string().contains("already exists"), "{taken}");
    let never = store
        .restore_file(&actor(), "JD-1", "never", 3)
        .unwrap_err();
    assert!(matches!(never, Error::FileNotFound { .. }), "{never}");
    assert_eq!(store.check().unwrap().problems(), 0);
    assert_rebuilds_identically(dir.path(), &mut store).unwrap();
}

// --- JD-14: the main file and other files never collide (MAJ-01) -------------

#[test]
fn renaming_onto_a_files_name_takes_the_id_folder_and_keeps_every_file() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap_with_files(&mut store).unwrap();
    store
        .create_file(&actor(), &new_file("backlog/notes", "nested\n", None))
        .unwrap();
    let renamed = store.rename(&actor(), "JD-1", "notes", None).unwrap();
    assert_eq!(renamed.path, "joedocs/plans/notes (JD-1)/notes (JD-1).md");
    let folder = "joedocs/plans/notes (JD-1)";
    assert!(read(dir.path(), &format!("{folder}/notes.md")).ends_with("---\nnotes\n"));
    assert!(read(dir.path(), &format!("{folder}/backlog/notes.md")).ends_with("---\nnested\n"));
    assert!(read(dir.path(), &renamed.path).ends_with("---\n# Roadmap\n"));
    let upper = store.rename(&actor(), "JD-1", "NOTES", None).unwrap();
    assert_eq!(upper.path, "joedocs/plans/NOTES (JD-1)/NOTES (JD-1).md");
    assert!(read(dir.path(), "joedocs/plans/NOTES (JD-1)/notes.md").ends_with("---\nnotes\n"));
    assert_eq!(store.check().unwrap().problems(), 0);
    assert_rebuilds_identically(dir.path(), &mut store).unwrap();
}

#[test]
fn a_rename_with_no_free_name_is_refused_and_changes_nothing() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap_with_files(&mut store).unwrap();
    store
        .create_file(&actor(), &new_file("notes (JD-1)", "x\n", None))
        .unwrap();
    let before = snapshot(dir.path()).unwrap();
    let tree = vault_tree(dir.path());
    let err = store.rename(&actor(), "JD-1", "Notes", None).unwrap_err();
    assert!(matches!(err, Error::AlreadyExists(_)), "{err}");
    assert_eq!(snapshot(dir.path()).unwrap(), before);
    assert_eq!(vault_tree(dir.path()), tree);
}

#[test]
fn unarchive_never_picks_a_folder_named_like_a_returning_file() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap_with_files(&mut store).unwrap();
    store
        .create_file(&actor(), &new_file("Roadmap (JD-1)", "x\n", None))
        .unwrap();
    store.archive(&actor(), "JD-1", "done").unwrap();
    // Another document takes the folder name.
    backlog_named(&mut store, "Roadmap").unwrap();
    let before = snapshot(dir.path()).unwrap();
    let err = store.unarchive(&actor(), "JD-1").unwrap_err();
    assert!(matches!(err, Error::AlreadyExists(_)), "{err}");
    assert_eq!(snapshot(dir.path()).unwrap(), before);
}

/// A multi-file document titled `title` in `joedocs/plans`.
fn backlog_named(store: &mut Store, title: &str) -> TestResult<()> {
    store.create(
        &actor(),
        NewDocument {
            project: "joedocs".into(),
            doc_type: Some("plans".into()),
            title: title.into(),
            content: "x\n".into(),
            multifile: true,
            ..NewDocument::default()
        },
    )?;
    Ok(())
}

#[test]
fn restores_refuse_a_file_named_like_the_current_main_file() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap_with_files(&mut store).unwrap();
    store
        .rename_file(&actor(), "JD-1", "notes", "n2", None)
        .unwrap();
    let renamed = store.rename(&actor(), "JD-1", "notes", None).unwrap();
    assert_eq!(renamed.path, "joedocs/plans/notes/notes.md");
    let before = snapshot(dir.path()).unwrap();
    let tree = vault_tree(dir.path());
    let folder = store
        .restore_version(&actor(), "JD-1", 3, None, None)
        .unwrap_err();
    assert!(matches!(folder, Error::AlreadyExists(_)), "{folder}");
    let one = store
        .restore_file(&actor(), "JD-1", "notes", 3)
        .unwrap_err();
    assert!(matches!(one, Error::AlreadyExists(_)), "{one}");
    assert_eq!(snapshot(dir.path()).unwrap(), before);
    assert_eq!(vault_tree(dir.path()), tree);
    assert!(read(dir.path(), "joedocs/plans/notes/notes.md").ends_with("---\n# Roadmap\n"));
}

// --- JD-14: restores never overwrite unknown files (MAJ-02) -----------------

/// Asserts `run` is refused with `already_exists` and leaves the planted
/// file, the index and the vault tree as they were.
fn assert_refused_keeping(
    root: &Path,
    planted: &str,
    run: impl FnOnce() -> Result<joedocs::store::Changed, Error>,
) -> TestResult<()> {
    let before = snapshot(root)?;
    let tree = vault_tree(root);
    let err = run().err().ok_or("the restore succeeded")?;
    assert!(matches!(err, Error::AlreadyExists(_)), "{err}");
    assert!(err.to_string().contains("does not know"), "{err}");
    assert_eq!(read(root, planted), "mine\n");
    assert_eq!(snapshot(root)?, before);
    assert_eq!(vault_tree(root), tree);
    Ok(())
}

#[test]
fn a_folder_restore_refuses_an_unknown_file_where_a_file_returns() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap_with_files(&mut store).unwrap();
    store
        .archive_file(&actor(), "JD-1", "notes", None, None)
        .unwrap();
    let planted = "joedocs/plans/Roadmap/notes.md";
    fs::write(dir.path().join("vault").join(planted), "mine\n").unwrap();
    assert_refused_keeping(dir.path(), planted, || {
        store.restore_version(&actor(), "JD-1", 3, None, None)
    })
    .unwrap();
}

#[test]
fn restores_refuse_an_unknown_file_where_a_renamed_file_returns() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap_with_files(&mut store).unwrap();
    store
        .rename_file(&actor(), "JD-1", "notes", "n2", None)
        .unwrap();
    let planted = "joedocs/plans/Roadmap/notes.md";
    fs::write(dir.path().join("vault").join(planted), "mine\n").unwrap();
    assert_refused_keeping(dir.path(), planted, || {
        store.restore_version(&actor(), "JD-1", 3, None, None)
    })
    .unwrap();
    assert_refused_keeping(dir.path(), planted, || {
        store.restore_file(&actor(), "JD-1", "notes", 3)
    })
    .unwrap();
}

#[test]
fn archive_restore_never_writes_over_an_unknown_file() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap_with_files(&mut store).unwrap();
    store.archive(&actor(), "JD-1", "done").unwrap();
    let planted = dir.path().join("vault/joedocs/plans/Roadmap/notes.md");
    fs::create_dir_all(planted.parent().unwrap()).unwrap();
    fs::write(&planted, "mine\n").unwrap();
    let back = store.unarchive(&actor(), "JD-1").unwrap();
    assert_eq!(back.path, "joedocs/plans/Roadmap (JD-1)/Roadmap (JD-1).md");
    assert_eq!(fs::read_to_string(&planted).unwrap(), "mine\n");
    assert!(read(dir.path(), "joedocs/plans/Roadmap (JD-1)/notes.md").ends_with("---\nnotes\n"));
}

// --- JD-14: restores respect documents.max_files (MED-02) -------------------

fn with_max_files(root: &Path, max_files: u64) -> Result<Store, Error> {
    let mut settings = Settings::from_config(&Config::default());
    settings.max_files = max_files;
    open_with(root, settings)
}

#[test]
fn folder_restore_and_unarchive_respect_the_file_limit() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap_with_files(&mut store).unwrap();
    store
        .archive_file(&actor(), "JD-1", "notes", None, None)
        .unwrap();
    drop(store);

    let mut store = with_max_files(dir.path(), 1).unwrap();
    let before = snapshot(dir.path()).unwrap();
    let err = store
        .restore_version(&actor(), "JD-1", 3, None, None)
        .unwrap_err();
    assert!(err.to_string().contains("documents.max_files"), "{err}");
    assert_eq!(snapshot(dir.path()).unwrap(), before);
    drop(store);
    let mut store = with_max_files(dir.path(), 2).unwrap();
    assert_eq!(
        store
            .restore_version(&actor(), "JD-1", 3, None, None)
            .unwrap()
            .revision,
        5
    );
    store.archive(&actor(), "JD-1", "done").unwrap();
    drop(store);

    let mut store = with_max_files(dir.path(), 1).unwrap();
    let before = snapshot(dir.path()).unwrap();
    let tree = vault_tree(dir.path());
    let err = store.unarchive(&actor(), "JD-1").unwrap_err();
    assert!(err.to_string().contains("documents.max_files"), "{err}");
    assert_eq!(snapshot(dir.path()).unwrap(), before);
    assert_eq!(vault_tree(dir.path()), tree);
    drop(store);
    let mut store = with_max_files(dir.path(), 2).unwrap();
    store.unarchive(&actor(), "JD-1").unwrap();
    assert_eq!(store.list_files("JD-1", None).unwrap().len(), 3);
}

// --- JD-14: converted documents keep their main file in earlier revisions ----

#[test]
fn a_converted_document_shows_its_main_file_before_the_conversion() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap(&mut store).unwrap();
    // JD-2 "Plan": 1 create, 2 append, 3 rename to "Plan B", 4 convert, 5 a file.
    store
        .append(&actor(), "JD-2", "more\n", None, None)
        .unwrap();
    store.rename(&actor(), "JD-2", "Plan B", None).unwrap();
    store.convert(&actor(), "JD-2").unwrap();
    let mut file = new_file("notes", "x\n", None);
    file.document_id = "JD-2".into();
    store.create_file(&actor(), &file).unwrap();

    let at = |version: u64| -> Vec<(String, bool, u64)> {
        store
            .list_files("JD-2", Some(version))
            .unwrap()
            .into_iter()
            .map(|f| (f.file, f.main, f.revision))
            .collect()
    };
    assert_eq!(at(1), [("Plan".to_owned(), true, 1)]);
    assert_eq!(at(2), [("Plan".to_owned(), true, 2)]);
    assert_eq!(at(3), [("Plan B".to_owned(), true, 3)]);
    assert_eq!(at(4), [("Plan B".to_owned(), true, 4)]);
    let main: Vec<u64> = store
        .history_file("JD-2", "Plan B", 1, 50)
        .unwrap()
        .revisions
        .iter()
        .map(|r| r.revision)
        .collect();
    assert_eq!(main, [4, 3, 2, 1]);
    let diff = store.diff("JD-2", 1, None, 0).unwrap();
    assert_eq!(diff.files, ["Plan B", "notes"]);
    assert!(diff.diff.contains("+more"), "{}", diff.diff);
    assert_rebuilds_identically(dir.path(), &mut store).unwrap();
}

// --- JD-16: destinations are checked without following symlinks -------------

/// Plants a symlink to nowhere at `path` (vault-relative).
fn dangling(root: &Path, path: &str) -> std::path::PathBuf {
    let link = root.join("vault").join(path);
    if let Some(parent) = link.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = std::os::unix::fs::symlink("nowhere", &link);
    link
}

fn assert_link_refused<T: std::fmt::Debug>(
    link: &Path,
    result: Result<T, Error>,
) -> TestResult<()> {
    let err = result.err().ok_or("the change succeeded")?;
    assert!(matches!(err, Error::AlreadyExists(_)), "{err}");
    assert_eq!(fs::read_link(link)?, Path::new("nowhere"));
    Ok(())
}

#[test]
fn a_dangling_symlink_at_a_file_destination_is_never_replaced() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap_with_files(&mut store).unwrap();
    backlog(&mut store).unwrap();
    let root = dir.path();

    let link = dangling(root, "joedocs/plans/Roadmap/fresh.md");
    assert_link_refused(
        &link,
        store.create_file(&actor(), &new_file("fresh", "x\n", None)),
    )
    .unwrap();
    let write = joedocs::store::WriteFiles {
        document_id: "JD-1".into(),
        files: vec![write("fresh", "x\n", json!({}))],
        expected_revision: None,
        summary: None,
    };
    assert_link_refused(&link, store.write_files(&actor(), &write)).unwrap();
    assert_link_refused(
        &link,
        store.rename_file(&actor(), "JD-1", "notes", "fresh", None),
    )
    .unwrap();
    let target = dangling(root, "joedocs/plans/Backlog/notes.md");
    assert_link_refused(
        &target,
        store.move_file(&actor(), "JD-1", "notes", "JD-3", None),
    )
    .unwrap();

    store
        .archive_file(&actor(), "JD-1", "notes", None, None)
        .unwrap();
    let restored = dangling(root, "joedocs/plans/Roadmap/notes.md");
    assert_link_refused(&restored, store.restore_file(&actor(), "JD-1", "notes", 3)).unwrap();
}

#[test]
fn a_dangling_symlink_makes_a_document_path_taken() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap(&mut store).unwrap();
    let link = dangling(dir.path(), "joedocs/plans/Other.md");
    let renamed = store.rename(&actor(), "JD-2", "Other", None).unwrap();
    assert_eq!(renamed.path, "joedocs/plans/Other (JD-2).md");
    assert_eq!(fs::read_link(&link).unwrap(), Path::new("nowhere"));
    let folder = dangling(dir.path(), "joedocs/plans/Fresh");
    backlog_named(&mut store, "Fresh").unwrap();
    assert!(
        dir.path()
            .join("vault/joedocs/plans/Fresh (JD-3)/Fresh (JD-3).md")
            .is_file()
    );
    assert_eq!(fs::read_link(&folder).unwrap(), Path::new("nowhere"));
}

/// Whether the vault's filesystem treats names that differ only in case as
/// one (macOS), or not (Linux).
fn folds_case(root: &Path) -> bool {
    let probe = root.join("vault/case-probe");
    let _ = fs::write(&probe, "");
    let folds = root.join("vault/CASE-PROBE").exists();
    let _ = fs::remove_file(probe);
    folds
}

#[test]
fn case_only_restores_need_the_same_file() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap_with_files(&mut store).unwrap();
    store
        .rename_file(&actor(), "JD-1", "notes", "Notes", None)
        .unwrap();
    if folds_case(dir.path()) {
        // One file under both spellings: the restores bring `notes` back.
        let folder = store
            .restore_version(&actor(), "JD-1", 3, None, None)
            .unwrap();
        assert_eq!(folder.revision, 5);
        let names: Vec<String> = store
            .list_files("JD-1", None)
            .unwrap()
            .into_iter()
            .map(|f| f.file)
            .collect();
        assert!(names.contains(&"notes".to_owned()), "{names:?}");
        return;
    }
    let planted = "joedocs/plans/Roadmap/notes.md";
    fs::write(dir.path().join("vault").join(planted), "mine\n").unwrap();
    assert_refused_keeping(dir.path(), planted, || {
        store.restore_version(&actor(), "JD-1", 3, None, None)
    })
    .unwrap();
    assert_refused_keeping(dir.path(), planted, || {
        store.restore_file(&actor(), "JD-1", "notes", 3)
    })
    .unwrap();
    // Without the unknown file, both restores rename `Notes` back.
    fs::remove_file(dir.path().join("vault").join(planted)).unwrap();
    store.restore_file(&actor(), "JD-1", "notes", 3).unwrap();
    assert!(read(dir.path(), "joedocs/plans/Roadmap/notes.md").ends_with("---\nnotes\n"));
    assert!(
        !dir.path()
            .join("vault/joedocs/plans/Roadmap/Notes.md")
            .exists()
    );
    assert_rebuilds_identically(dir.path(), &mut store).unwrap();
}

#[test]
fn case_only_renames_need_the_same_file() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    roadmap_with_files(&mut store).unwrap();
    store
        .rename_file(&actor(), "JD-1", "notes", "Notes", None)
        .unwrap();
    let folds = folds_case(dir.path());
    let vault = dir.path().join("vault");
    if folds {
        let renamed = store
            .rename_file(&actor(), "JD-1", "Notes", "notes", None)
            .unwrap();
        assert_eq!(renamed.file.as_deref(), Some("notes"));
    } else {
        fs::write(vault.join("joedocs/plans/Roadmap/notes.md"), "mine\n").unwrap();
        fs::write(vault.join("joedocs/plans/plan.md"), "mine\n").unwrap();
        fs::create_dir(vault.join("joedocs/plans/roadmap")).unwrap();
        fs::write(vault.join("joedocs/plans/roadmap/x.md"), "mine\n").unwrap();
        // A file: refused, nothing changes.
        assert_refused_keeping(dir.path(), "joedocs/plans/Roadmap/notes.md", || {
            store.rename_file(&actor(), "JD-1", "Notes", "notes", None)
        })
        .unwrap();
    }
    // Documents: a different file takes the (ID) name; the same file renames in place.
    let single = store.rename(&actor(), "JD-2", "plan", None).unwrap();
    let folder = store.rename(&actor(), "JD-1", "roadmap", None).unwrap();
    if folds {
        assert_eq!(single.path, "joedocs/plans/plan.md");
        assert_eq!(folder.path, "joedocs/plans/roadmap/roadmap.md");
    } else {
        assert_eq!(single.path, "joedocs/plans/plan (JD-2).md");
        assert_eq!(
            folder.path,
            "joedocs/plans/roadmap (JD-1)/roadmap (JD-1).md"
        );
        assert_eq!(read(dir.path(), "joedocs/plans/plan.md"), "mine\n");
        assert_eq!(read(dir.path(), "joedocs/plans/roadmap/x.md"), "mine\n");
        assert_eq!(read(dir.path(), "joedocs/plans/Roadmap/notes.md"), "mine\n");
        assert!(
            read(dir.path(), "joedocs/plans/roadmap (JD-1)/Notes.md").ends_with("---\nnotes\n")
        );
    }
    assert_rebuilds_identically(dir.path(), &mut store).unwrap();
}
