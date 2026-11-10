//! Phase 4 store operations against a temporary vault and real git:
//! history, diffs, restoring revisions, moving, exact edits, appending,
//! archive listing and restore, and `check`.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use joedocs::Config;
use joedocs::clock::FixedClock;
use joedocs::document::front_matter::Fields;
use joedocs::history::GitCli;
use joedocs::store::{Actor, Error, NewDocument, Paths, Replacement, Settings, Store, Update};
use serde_json::{Value, json};
use tempfile::TempDir;

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

struct Fixture {
    dir: TempDir,
    clock: Arc<FixedClock>,
    store: Store,
}

fn paths(root: &Path) -> Paths {
    Paths {
        home: root.join("vault"),
        state_dir: root.join("state"),
    }
}

fn fields(value: &Value) -> Fields {
    value
        .as_object()
        .map(|map| map.clone().into_iter().collect())
        .unwrap_or_default()
}

fn actor() -> Actor {
    Actor::new("laptop", "claude-code")
}

/// Project `joedocs` with JD-1 "Plan" (an idea, tagged, body "Step one.").
fn fixture() -> TestResult<Fixture> {
    let dir = TempDir::new()?;
    let paths = paths(dir.path());
    let git = GitCli::new(paths.git_dir(), &paths.home);
    Store::init(&paths, &git)?;
    let clock = Arc::new(FixedClock::at(1_793_000_000));
    let mut store = Store::open(
        paths,
        Settings::from_config(&Config::default()),
        Box::new(git),
        Arc::clone(&clock) as Arc<dyn joedocs::clock::Clock>,
    )?;
    store.add_project("joedocs", "JD", None)?;
    store.create(
        &actor(),
        NewDocument {
            project: "joedocs".into(),
            doc_type: Some("ideas".into()),
            title: "Plan".into(),
            content: "# Plan\n\nStep one.\n".into(),
            front_matter: fields(&json!({"tags": ["draft-idea"], "owner": "joel"})),
            ..NewDocument::default()
        },
    )?;
    Ok(Fixture { dir, clock, store })
}

fn body(fx: &Fixture, id: &str) -> TestResult<String> {
    let markdown = fx.store.get(id, None)?.markdown;
    let parsed = joedocs::document::front_matter::parse(&markdown)?;
    Ok(parsed.body.to_owned())
}

fn edit(old: &str, new: &str) -> Replacement {
    Replacement {
        old: old.into(),
        new: new.into(),
    }
}

fn invalid(result: Result<impl std::fmt::Debug, Error>) -> (String, String) {
    match result {
        Err(Error::Invalid { field, message }) => (field.to_owned(), message),
        other => (format!("not invalid: {other:?}"), String::new()),
    }
}

#[test]
fn edits_replace_exact_text_in_the_body() {
    let mut fx = fixture().unwrap();
    let changed = fx
        .store
        .edit(
            &actor(),
            "JD-1",
            &[
                edit("Step one.", "Step one.\nStep two."),
                edit("# Plan", "# The plan"),
            ],
            Some(1),
            Some("add a step"),
        )
        .unwrap();
    assert_eq!(changed.revision, 2);
    assert_eq!(
        body(&fx, "JD-1").unwrap(),
        "# The plan\n\nStep one.\nStep two.\n"
    );
    let history = fx.store.history("JD-1", 1, 10).unwrap();
    assert_eq!(history.revisions[0].action, "edit");
    assert_eq!(history.revisions[0].summary, "Edit JD-1: add a step");

    // A failing edit changes nothing, and names the edit.
    let (field, message) = invalid(fx.store.edit(
        &actor(),
        "JD-1",
        &[edit("Step two.", "Step 2."), edit("Step", "x")],
        None,
        None,
    ));
    assert_eq!(field, "edits");
    assert!(
        message.starts_with("edit 2: old occurs 2 times"),
        "{message}"
    );
    // Front matter is not part of the body.
    let (_, message) =
        invalid(
            fx.store
                .edit(&actor(), "JD-1", &[edit("owner: joel", "x")], None, None),
        );
    assert!(message.contains("not found"), "{message}");
    assert!(matches!(
        fx.store
            .edit(&actor(), "JD-1", &[edit("Step one.", "x")], Some(1), None),
        Err(Error::Conflict { current: 2, .. })
    ));
    assert_eq!(
        invalid(fx.store.edit(&actor(), "JD-1", &[], None, None)).0,
        "edits"
    );
    assert_eq!(fx.store.get("JD-1", None).unwrap().document.revision, 2);
}

#[test]
fn appends_add_a_section_at_the_end() {
    let mut fx = fixture().unwrap();
    fx.store
        .append(&actor(), "JD-1", "## Comment\n\nLooks good.", None, None)
        .unwrap();
    assert_eq!(
        body(&fx, "JD-1").unwrap(),
        "# Plan\n\nStep one.\n\n## Comment\n\nLooks good.\n"
    );
    assert_eq!(
        invalid(fx.store.append(&actor(), "JD-1", " \n", None, None)).0,
        "content"
    );
}

#[test]
fn moves_change_the_type_and_folder() {
    let mut fx = fixture().unwrap();
    let moved = fx
        .store
        .move_document(&actor(), "JD-1", "plans", None)
        .unwrap();
    assert_eq!(moved.path, "joedocs/plans/Plan.md");
    assert_eq!(moved.front_matter["type"], "plans");
    let home = paths(fx.dir.path()).home;
    assert!(home.join("joedocs/plans/Plan.md").is_file());
    assert!(
        !home.join("joedocs/ideas").exists(),
        "the emptied folder is removed"
    );
    let history = fx.store.history("JD-1", 1, 1).unwrap();
    assert_eq!(history.revisions[0].summary, "Move JD-1: ideas → plans");

    assert_eq!(
        invalid(fx.store.move_document(&actor(), "JD-1", "plans", None)).0,
        "type"
    );
    assert_eq!(
        invalid(fx.store.move_document(&actor(), "JD-1", "nope", None)).0,
        "type"
    );
}

#[test]
fn history_is_newest_first_and_paged() {
    let mut fx = fixture().unwrap();
    for n in 2..=4 {
        fx.clock.advance(60);
        fx.store
            .append(&actor(), "JD-1", &format!("Entry {n}."), None, None)
            .unwrap();
    }
    let page = fx.store.history("JD-1", 1, 3).unwrap();
    let revisions: Vec<u64> = page.revisions.iter().map(|r| r.revision).collect();
    assert_eq!(revisions, [4, 3, 2]);
    assert_eq!((page.total, page.next_start), (4, Some(4)));
    assert_eq!(page.revisions[0].at, "2026-10-26T07:36:20Z");
    assert_eq!(page.revisions[0].author, "laptop");
    let last = fx.store.history("JD-1", 4, 3).unwrap();
    assert_eq!(last.revisions[0].action, "create");
    assert_eq!(last.next_start, None);
    assert_eq!(invalid(fx.store.history("JD-1", 0, 3)).0, "start");
    assert_eq!(invalid(fx.store.history("JD-1", 1, 1001)).0, "limit");
    assert!(matches!(
        fx.store.history("JD-9", 1, 3),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn diffs_show_what_changed_between_revisions() {
    let mut fx = fixture().unwrap();
    fx.store
        .edit(
            &actor(),
            "JD-1",
            &[edit("Step one.", "Step 1.")],
            None,
            None,
        )
        .unwrap();
    let diff = fx.store.diff("JD-1", 1, None, 0).unwrap();
    assert_eq!(diff.to, 2);
    assert!(
        diff.diff
            .starts_with("--- JD-1 revision 1\n+++ JD-1 revision 2\n@@"),
        "{}",
        diff.diff
    );
    assert!(
        diff.diff.contains("-revision: 1\n+revision: 2\n"),
        "{}",
        diff.diff
    );
    assert!(
        diff.diff.contains("-Step one.\n+Step 1.\n"),
        "{}",
        diff.diff
    );
    let reverse = fx.store.diff("JD-1", 2, Some(1), 0).unwrap();
    assert!(
        reverse.diff.contains("-Step 1.\n+Step one.\n"),
        "{}",
        reverse.diff
    );
    assert_eq!(fx.store.diff("JD-1", 2, Some(2), 3).unwrap().diff, "");
    assert_eq!(invalid(fx.store.diff("JD-1", 0, None, 3)).0, "from");
    assert_eq!(invalid(fx.store.diff("JD-1", 1, Some(3), 3)).0, "to");
    assert_eq!(invalid(fx.store.diff("JD-1", 1, None, 101)).0, "context");
}

#[test]
fn restoring_brings_back_body_and_front_matter_but_keeps_title_and_type() {
    let mut fx = fixture().unwrap();
    fx.store
        .update(
            &actor(),
            Update {
                id: "JD-1".into(),
                content: "Rewritten.\n".into(),
                expected_revision: 1,
                front_matter: fields(&json!({"status": "review", "tags": ["other"]})),
                summary: None,
            },
        )
        .unwrap();
    fx.store
        .change_front_matter(
            &actor(),
            "JD-1",
            &Fields::new(),
            &["owner".to_owned()],
            None,
        )
        .unwrap();
    fx.store
        .rename(&actor(), "JD-1", "Better plan", None)
        .unwrap();
    fx.store
        .move_document(&actor(), "JD-1", "plans", None)
        .unwrap();

    let restored = fx
        .store
        .restore_version(&actor(), "JD-1", 1, Some(5), None)
        .unwrap();
    assert_eq!(restored.revision, 6);
    assert_eq!(restored.path, "joedocs/plans/Better plan.md");
    let fm = &restored.front_matter;
    assert_eq!(
        (&fm["title"], &fm["type"], &fm["status"]),
        (&json!("Better plan"), &json!("plans"), &json!("draft"))
    );
    assert_eq!(fm["tags"], json!(["draft-idea"]));
    assert_eq!(fm["owner"], json!("joel"));
    assert_eq!(fm["aliases"], json!(["Plan"]));
    assert_eq!(body(&fx, "JD-1").unwrap(), "# Plan\n\nStep one.\n");
    let history = fx.store.history("JD-1", 1, 1).unwrap();
    assert_eq!(history.revisions[0].action, "restore");
    assert_eq!(history.revisions[0].summary, "Restore JD-1: revision 1");

    assert_eq!(
        invalid(fx.store.restore_version(&actor(), "JD-1", 6, None, None)).0,
        "version"
    );
    assert_eq!(
        invalid(fx.store.restore_version(&actor(), "JD-1", 9, None, None)).0,
        "version"
    );
    assert!(matches!(
        fx.store.restore_version(&actor(), "JD-1", 2, Some(1), None),
        Err(Error::Conflict { current: 6, .. })
    ));
}

#[test]
fn archived_documents_are_listed_and_restored() {
    let mut fx = fixture().unwrap();
    fx.store.add_project("infra", "IN", None).unwrap();
    for (project, title) in [("infra", "Cache"), ("joedocs", "Other")] {
        fx.store
            .create(
                &actor(),
                NewDocument {
                    project: project.into(),
                    title: title.into(),
                    content: "x\n".into(),
                    ..NewDocument::default()
                },
            )
            .unwrap();
    }
    assert!(matches!(
        fx.store.unarchive(&actor(), "JD-1"),
        Err(Error::Invalid { .. })
    ));
    fx.store.archive(&actor(), "JD-1", "superseded").unwrap();
    fx.clock.advance(60);
    fx.store.archive(&actor(), "IN-1", "moved").unwrap();

    let all = fx.store.archived(None, None).unwrap();
    let ids: Vec<&str> = all.iter().map(|a| a.document.id.as_str()).collect();
    assert_eq!(ids, ["IN-1", "JD-1"]);
    assert_eq!(
        (all[1].reason.as_str(), all[1].archived_by.as_str()),
        ("superseded", "laptop")
    );
    assert_eq!(fx.store.archived(Some("joedocs"), None).unwrap().len(), 1);
    assert!(matches!(
        fx.store.archived(Some("nope"), None),
        Err(Error::ProjectNotFound(_))
    ));

    // While JD-1 is archived, a new document takes its file name.
    fx.store
        .create(
            &actor(),
            NewDocument {
                project: "joedocs".into(),
                doc_type: Some("ideas".into()),
                title: "Plan".into(),
                content: "new\n".into(),
                ..NewDocument::default()
            },
        )
        .unwrap();
    let admin = Actor::new("admin", "joedocs-cli");
    let restored = fx.store.unarchive(&admin, "JD-1").unwrap();
    assert_eq!(restored.revision, 3);
    assert_eq!(restored.path, "joedocs/ideas/Plan (JD-1).md");
    assert_eq!(restored.front_matter["updated_by"], "admin");
    assert_eq!(restored.front_matter["owner"], "joel");
    assert_eq!(body(&fx, "JD-1").unwrap(), "# Plan\n\nStep one.\n");
    assert_eq!(fx.store.archived(None, None).unwrap().len(), 1);
    // The archive revision has no content to read or diff.
    assert_eq!(invalid(fx.store.get("JD-1", Some(2))).0, "version");
    assert_eq!(invalid(fx.store.diff("JD-1", 2, None, 3)).0, "from");
    assert_eq!(fx.store.diff("JD-1", 1, None, 0).unwrap().to, 3);
    assert!(matches!(
        fx.store.unarchive(&admin, "JD-9"),
        Err(Error::NotFound(_))
    ));

    // Every new action replays into the same index.
    let before = fx.store.history("JD-1", 1, 10).unwrap();
    fx.store.reindex().unwrap();
    assert_eq!(fx.store.history("JD-1", 1, 10).unwrap(), before);
    assert_eq!(fx.store.archived(None, None).unwrap().len(), 1);
}

#[test]
fn every_phase_4_action_replays_identically() {
    let mut fx = fixture().unwrap();
    fx.store
        .edit(&actor(), "JD-1", &[edit("one", "1")], None, None)
        .unwrap();
    fx.store
        .append(&actor(), "JD-1", "More.", None, None)
        .unwrap();
    fx.store
        .move_document(&actor(), "JD-1", "specs", None)
        .unwrap();
    fx.store
        .restore_version(&actor(), "JD-1", 2, None, None)
        .unwrap();
    let get = |store: &Store| store.get("JD-1", None).map(|f| (f.document, f.markdown));
    let before = (
        get(&fx.store).unwrap(),
        fx.store.history("JD-1", 1, 10).unwrap(),
    );
    let report = fx.store.reindex().unwrap();
    assert_eq!(report.revisions, 5);
    assert_eq!(
        (
            get(&fx.store).unwrap(),
            fx.store.history("JD-1", 1, 10).unwrap()
        ),
        before
    );
}

#[test]
fn check_reports_drift_and_unknown_files() {
    let fx = fixture().unwrap();
    let report = fx.store.check().unwrap();
    assert_eq!(report.problems(), 0, "{report:?}");

    let home = paths(fx.dir.path()).home;
    fs::write(home.join("joedocs/ideas/Plan.md"), "edited").unwrap();
    fs::write(home.join("stray.md"), "x").unwrap();
    fs::create_dir_all(home.join(".obsidian")).unwrap();
    fs::write(home.join(".obsidian/app.json"), "{}").unwrap();
    fs::create_dir_all(home.join("joedocs/notes")).unwrap();
    fs::write(home.join("joedocs/notes/Loose.md"), "x").unwrap();
    let report = fx.store.check().unwrap();
    assert!(!report.index_behind);
    assert_eq!(
        report.drifted,
        [("JD-1".to_owned(), "joedocs/ideas/Plan.md".to_owned())]
    );
    assert_eq!(report.unknown_files, ["joedocs/notes/Loose.md", "stray.md"]);
    assert_eq!(report.problems(), 3);

    let conn = joedocs::db::open(&paths(fx.dir.path()).db()).unwrap();
    conn.execute("DELETE FROM meta WHERE key = 'index_format'", [])
        .unwrap();
    assert!(fx.store.check().unwrap().index_behind);
}
