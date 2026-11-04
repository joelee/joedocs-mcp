//! Store unit tests: a temporary vault, real git, and a history that can be
//! made to fail.

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

use super::*;
use crate::auth::{CountingRandom, Permission};
use crate::clock::FixedClock;
use crate::document::front_matter::Fields;
use crate::history::{Commit, GitCli, Logged};

/// A history that delegates to git but can be told to fail commits.
struct Flaky {
    git: GitCli,
    fail: Arc<AtomicBool>,
}

impl History for Flaky {
    fn exists(&self) -> bool {
        self.git.exists()
    }
    fn init(&self) -> Result<(), HistoryError> {
        self.git.init()
    }
    fn head(&self) -> Result<Option<String>, HistoryError> {
        self.git.head()
    }
    fn commit(&self, commit: &Commit) -> Result<String, HistoryError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(HistoryError::Failed {
                command: "commit".into(),
                stderr: "injected".into(),
            });
        }
        self.git.commit(commit)
    }
    fn show(&self, id: &str, path: &str) -> Result<Vec<u8>, HistoryError> {
        self.git.show(id, path)
    }
    fn log(&self) -> Result<Vec<Logged>, HistoryError> {
        self.git.log()
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    paths: Paths,
    clock: Arc<FixedClock>,
    fail: Arc<AtomicBool>,
    store: Store,
}

fn git_for(paths: &Paths) -> GitCli {
    GitCli::new(paths.git_dir(), &paths.home)
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: dir.path().join("home"),
        state_dir: dir.path().join("state"),
    };
    assert_eq!(
        Store::init(&paths, &git_for(&paths)).unwrap(),
        InitOutcome::Created
    );
    let clock = Arc::new(FixedClock::at(1_793_000_000));
    let fail = Arc::new(AtomicBool::new(false));
    let history = Flaky {
        git: git_for(&paths),
        fail: Arc::clone(&fail),
    };
    let mut store = Store::open(
        paths.clone(),
        Settings::from_config(&Config::default()),
        Box::new(history),
        Arc::clone(&clock) as Arc<dyn Clock>,
    )
    .unwrap();
    store.add_project("joedocs", "JD", Some("JoeDocs")).unwrap();
    Fixture {
        _dir: dir,
        paths,
        clock,
        fail,
        store,
    }
}

fn actor() -> Actor {
    Actor::new("laptop", "claude-code")
}

fn fields(value: Value) -> Fields {
    match value {
        Value::Object(map) => map.into_iter().collect(),
        _ => Fields::new(),
    }
}

fn new_doc(title: &str) -> NewDocument {
    NewDocument {
        project: "joedocs".into(),
        doc_type: Some("plans".into()),
        title: title.into(),
        content: "Body\n".into(),
        ..NewDocument::default()
    }
}

fn read(fx: &Fixture, path: &str) -> String {
    fs::read_to_string(fx.paths.home.join(path)).unwrap()
}

fn git_log(fx: &Fixture) -> String {
    let output = Command::new("git")
        .arg("--git-dir")
        .arg(fx.paths.git_dir())
        .args([
            "log",
            "--format=%an|%s|%(trailers:key=JoeDocs-Action,valueonly)",
        ])
        .output()
        .unwrap();
    String::from_utf8(output.stdout)
        .unwrap()
        .replace("\n\n", "\n")
}

fn update(id: &str, content: &str, expected: u64) -> Update {
    Update {
        id: id.into(),
        content: content.into(),
        expected_revision: expected,
        ..Update::default()
    }
}

#[test]
fn init_is_idempotent_and_refuses_a_foreign_directory() {
    let fx = fixture();
    let git = git_for(&fx.paths);
    assert_eq!(
        Store::init(&fx.paths, &git).unwrap(),
        InitOutcome::AlreadyInitialised
    );

    let dir = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: dir.path().join("home"),
        state_dir: dir.path().join("state"),
    };
    fs::create_dir_all(&paths.home).unwrap();
    fs::write(paths.home.join("notes.md"), "mine").unwrap();
    let err = Store::init(&paths, &git_for(&paths)).unwrap_err();
    assert!(matches!(err, Error::HomeNotEmpty(_)), "{err}");
    assert!(err.is_user_error());
}

#[test]
fn open_requires_init() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths {
        home: dir.path().join("home"),
        state_dir: dir.path().join("state"),
    };
    let err = Store::open(
        paths.clone(),
        Settings::from_config(&Config::default()),
        Box::new(git_for(&paths)),
        Arc::new(FixedClock::at(0)),
    )
    .unwrap_err();
    assert!(err.to_string().contains("run `joedocs init`"), "{err}");
}

#[test]
fn create_writes_the_file_commits_and_indexes_it() {
    let mut fx = fixture();
    let mut request = new_doc("Initial plan: v1");
    request.front_matter = fields(json!({"owner": "joel", "tags": ["mvp"]}));
    let created = fx.store.create(&actor(), request).unwrap();
    assert_eq!(created.id, "JD-1");
    assert_eq!(created.revision, 1);
    assert_eq!(created.path, "joedocs/plans/Initial plan v1.md");
    assert_eq!(created.front_matter["owner"], json!("joel"));

    assert_eq!(
        read(&fx, &created.path),
        "---\nid: JD-1\ntitle: \"Initial plan: v1\"\nproject: joedocs\ntype: plans\n\
         status: draft\nrevision: 1\ncreated: \"2026-10-26T07:33:20Z\"\ncreated_by: laptop\n\
         updated: \"2026-10-26T07:33:20Z\"\nupdated_by: laptop\nclient: claude-code\n\
         tags:\n- mvp\nowner: joel\n---\nBody\n"
    );
    assert_eq!(
        git_log(&fx),
        "laptop|Create JD-1: Initial plan: v1|create\n"
    );
    let dates = Command::new("git")
        .arg("--git-dir")
        .arg(fx.paths.git_dir())
        .args(["log", "--format=%aI %cI"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(dates.stdout).unwrap(),
        "2026-10-26T07:33:20Z 2026-10-26T07:33:20Z\n",
        "commits carry the revision time"
    );

    let fetched = fx.store.get("jd-1", None).unwrap();
    assert_eq!(fetched.markdown, read(&fx, &created.path));
    assert_eq!(fetched.document.title, "Initial plan: v1");
    assert_eq!(fetched.revision.action, "create");
    assert_eq!(fetched.revision.summary, "Create JD-1: Initial plan: v1");
    assert_eq!(fx.store.projects().unwrap()[0].documents, 1);
}

#[test]
fn create_uses_defaults_and_numbers_documents_per_project() {
    let mut fx = fixture();
    fx.store.add_project("alpha", "AL", None).unwrap();
    let mut request = new_doc("Home");
    request.doc_type = None;
    request.summary = Some("  first\n  page ".into());
    let first = fx.store.create(&actor(), request).unwrap();
    assert_eq!(first.path, "joedocs/home/Home.md");
    assert_eq!(first.front_matter["status"], json!("draft"));
    assert!(git_log(&fx).starts_with("laptop|Create JD-1: first page|"));

    let mut other = new_doc("Home");
    other.project = "alpha".into();
    assert_eq!(fx.store.create(&actor(), other).unwrap().id, "AL-1");
    assert_eq!(
        fx.store.create(&actor(), new_doc("Two")).unwrap().id,
        "JD-2"
    );
}

#[test]
fn create_validates_its_arguments() {
    let mut fx = fixture();
    let invalid = |fx: &mut Fixture, request: NewDocument| {
        let err = fx.store.create(&actor(), request).unwrap_err();
        assert!(err.is_user_error(), "{err}");
        err.to_string()
    };
    let mut r = new_doc("x");
    r.doc_type = Some("memo".into());
    assert!(invalid(&mut fx, r).starts_with("invalid type: \"memo\" is not one of: home,"));
    let mut r = new_doc("x");
    r.status = Some("open".into());
    assert!(invalid(&mut fx, r).starts_with("invalid status:"));
    assert_eq!(
        invalid(&mut fx, new_doc(" ")),
        "invalid title: must not be empty"
    );
    let mut r = new_doc("x");
    r.front_matter = fields(json!({"status": "done"}));
    assert!(invalid(&mut fx, r).contains("its own parameter"));
    let mut r = new_doc("x");
    r.front_matter = fields(json!({"id": "JD-9"}));
    assert!(invalid(&mut fx, r).contains("owned by the server"));
    let mut r = new_doc("x");
    r.project = "nope".into();
    assert_eq!(invalid(&mut fx, r), "project nope does not exist");
    let mut r = new_doc("x");
    r.project = "JD".into();
    assert_eq!(invalid(&mut fx, r), "project JD does not exist");
    let mut r = new_doc("x");
    r.content = "x".repeat(2 * 1024 * 1024);
    assert!(invalid(&mut fx, r).contains("server.max_document_bytes"));

    assert_eq!(git_log(&fx), "");
    assert_eq!(fs::read_dir(&fx.paths.home).unwrap().count(), 0);
}

#[test]
fn clashing_file_names_get_the_id_appended() {
    let mut fx = fixture();
    fx.store.create(&actor(), new_doc("Plan")).unwrap();
    let second = fx.store.create(&actor(), new_doc("plan")).unwrap();
    assert_eq!(second.path, "joedocs/plans/plan (JD-2).md");

    fs::write(fx.paths.home.join("joedocs/plans/Stray.md"), "not ours").unwrap();
    let third = fx.store.create(&actor(), new_doc("Stray")).unwrap();
    assert_eq!(third.path, "joedocs/plans/Stray (JD-3).md");
    assert_eq!(read(&fx, "joedocs/plans/Stray.md"), "not ours");
}

#[test]
fn update_replaces_the_body_and_merges_front_matter() {
    let mut fx = fixture();
    let mut request = new_doc("Plan");
    request.front_matter = fields(json!({"owner": "joel"}));
    let created = fx.store.create(&actor(), request).unwrap();
    fx.clock.advance(60);

    let mut change = update("JD-1", "New body\n", 1);
    change.front_matter = fields(json!({"status": "review", "due": "2026-12-01"}));
    change.summary = Some("tighten scope".into());
    let other = Actor::new("desktop", "");
    let updated = fx.store.update(&other, change).unwrap();
    assert_eq!(updated.revision, 2);
    assert_eq!(updated.path, created.path);

    let text = read(&fx, &created.path);
    assert!(
        text.ends_with("owner: joel\ndue: \"2026-12-01\"\n---\nNew body\n"),
        "{text}"
    );
    assert!(text.contains("status: review\nrevision: 2\n"), "{text}");
    assert!(text.contains("created_by: laptop\nupdated: \"2026-10-26T07:34:20Z\"\nupdated_by: desktop\nclient: unknown\n"), "{text}");
    assert!(git_log(&fx).starts_with("desktop|Update JD-1: tighten scope|update\n"));

    let old = fx.store.get("JD-1", Some(1)).unwrap();
    assert!(old.markdown.ends_with("---\nBody\n"));
    assert_eq!(old.revision.revision, 1);
    assert_eq!(old.document.revision, 2);
    assert_eq!(old.document.status, "review");
}

#[test]
fn stale_revisions_conflict() {
    let mut fx = fixture();
    fx.store.create(&actor(), new_doc("Plan")).unwrap();
    fx.store.update(&actor(), update("JD-1", "a", 1)).unwrap();
    let err = fx
        .store
        .update(&actor(), update("JD-1", "b", 1))
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "document JD-1 is at revision 2, not 1; read it again and reapply the change"
    );
    let err = fx
        .store
        .rename(&actor(), "JD-1", "New", Some(7))
        .unwrap_err();
    assert!(matches!(err, Error::Conflict { current: 2, .. }), "{err}");
}

#[test]
fn files_edited_behind_the_stores_back_are_not_overwritten() {
    let mut fx = fixture();
    let created = fx.store.create(&actor(), new_doc("Plan")).unwrap();
    let file = fx.paths.home.join(&created.path);
    let committed = fs::read_to_string(&file).unwrap();
    fs::write(&file, "edited in place\n").unwrap();

    let err = fx
        .store
        .update(&actor(), update("JD-1", "x", 1))
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "document JD-1: joedocs/plans/Plan.md was changed or removed outside JoeDocs, so it will not be overwritten"
    );
    assert!(!err.is_user_error());
    assert_eq!(fs::read_to_string(&file).unwrap(), "edited in place\n");
    assert!(matches!(
        fx.store.archive(&actor(), "JD-1", "old"),
        Err(Error::Integrity { .. })
    ));
    assert_eq!(git_log(&fx).lines().count(), 1);

    let fetched = fx.store.get("JD-1", None).unwrap();
    assert_eq!(
        fetched.markdown, committed,
        "reads fall back to the history"
    );

    fs::remove_file(&file).unwrap();
    assert!(matches!(
        fx.store
            .change_front_matter(&actor(), "JD-1", &fields(json!({"a": 1})), &[], None),
        Err(Error::Integrity { .. })
    ));
}

#[test]
fn rename_moves_the_file_and_keeps_the_old_title_as_an_alias() {
    let mut fx = fixture();
    fx.store.create(&actor(), new_doc("Draft plan")).unwrap();
    let renamed = fx
        .store
        .rename(&actor(), "JD-1", "Final plan", Some(1))
        .unwrap();
    assert_eq!(renamed.path, "joedocs/plans/Final plan.md");
    assert_eq!(renamed.front_matter["aliases"], json!(["Draft plan"]));
    assert!(!fx.paths.home.join("joedocs/plans/Draft plan.md").exists());
    assert!(read(&fx, &renamed.path).contains("title: Final plan\n"));
    assert!(git_log(&fx).starts_with("laptop|Rename JD-1: Draft plan → Final plan|rename\n"));

    let back = fx
        .store
        .rename(&actor(), "JD-1", "Draft plan", None)
        .unwrap();
    assert_eq!(back.front_matter["aliases"], json!(["Final plan"]));

    let case = fx
        .store
        .rename(&actor(), "JD-1", "draft plan", None)
        .unwrap();
    assert_eq!(case.path, "joedocs/plans/draft plan.md");
    let names: Vec<_> = fs::read_dir(fx.paths.home.join("joedocs/plans"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, ["draft plan.md"]);

    let same_file = fx
        .store
        .rename(&actor(), "JD-1", "draft plan?", None)
        .unwrap();
    assert_eq!(same_file.path, "joedocs/plans/draft plan.md");

    let err = fx
        .store
        .rename(&actor(), "JD-1", "draft plan?", None)
        .unwrap_err();
    assert!(
        err.to_string().contains("already the document's title"),
        "{err}"
    );
    let old = fx.store.get("JD-1", Some(1)).unwrap();
    assert!(old.markdown.contains("title: Draft plan\n"));
    assert_eq!(old.revision.path, "joedocs/plans/Draft plan.md");
}

#[test]
fn front_matter_keys_can_be_set_and_removed() {
    let mut fx = fixture();
    fx.store.create(&actor(), new_doc("Plan")).unwrap();
    let set = fields(json!({"owner": "joel", "status": "approved", "tags": ["a"]}));
    let changed = fx
        .store
        .change_front_matter(&actor(), "JD-1", &set, &[], Some(1))
        .unwrap();
    assert_eq!(changed.front_matter["status"], json!("approved"));
    let changed = fx
        .store
        .change_front_matter(
            &actor(),
            "JD-1",
            &Fields::new(),
            &["owner".to_owned()],
            None,
        )
        .unwrap();
    assert!(!changed.front_matter.contains_key("owner"));
    assert_eq!(changed.revision, 3);
    assert!(read(&fx, &changed.path).ends_with("tags:\n- a\n---\nBody\n"));
    assert!(git_log(&fx).starts_with("laptop|Change front matter of JD-1|front_matter\n"));

    let err = fx
        .store
        .change_front_matter(&actor(), "JD-1", &Fields::new(), &[], None)
        .unwrap_err();
    assert!(err.to_string().contains("nothing to change"), "{err}");
    let err = fx
        .store
        .change_front_matter(&actor(), "JD-1", &Fields::new(), &["tags".into()], None)
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid front_matter: \"tags\" cannot be removed"
    );
    let err = fx
        .store
        .update(
            &actor(),
            Update {
                front_matter: fields(json!({"revision": 1})),
                ..update("JD-1", "x", 3)
            },
        )
        .unwrap_err();
    assert!(err.to_string().contains("owned by the server"), "{err}");
}

#[test]
fn archive_removes_the_file_but_keeps_the_history() {
    let mut fx = fixture();
    let created = fx.store.create(&actor(), new_doc("Plan")).unwrap();
    let err = fx.store.archive(&actor(), "JD-1", "  ").unwrap_err();
    assert_eq!(err.to_string(), "invalid reason: must not be empty");

    let archived = fx
        .store
        .archive(&actor(), "JD-1", "superseded by JD-2")
        .unwrap();
    assert_eq!(archived.revision, 2);
    assert_eq!(archived.archived_at, "2026-10-26T07:33:20Z");
    assert!(
        !fx.paths.home.join("joedocs").exists(),
        "empty folders are removed"
    );
    assert!(git_log(&fx).starts_with("laptop|Archive JD-1: superseded by JD-2|archive\n"));
    assert_eq!(fx.store.projects().unwrap()[0].documents, 0);

    assert_eq!(
        fx.store.get("JD-1", None).unwrap_err().to_string(),
        "document JD-1 is archived"
    );
    assert!(matches!(
        fx.store.update(&actor(), update("JD-1", "x", 2)),
        Err(Error::Archived(_))
    ));
    let again = fx.store.create(&actor(), new_doc("Plan")).unwrap();
    assert_eq!(
        again.path, created.path,
        "archived documents free their path"
    );
}

#[test]
fn get_reports_missing_documents_and_revisions() {
    let mut fx = fixture();
    fx.store.create(&actor(), new_doc("Plan")).unwrap();
    assert_eq!(
        fx.store.get("JD-9", None).unwrap_err().to_string(),
        "document JD-9 does not exist"
    );
    assert_eq!(
        fx.store.get("JD-1", Some(2)).unwrap_err().to_string(),
        "invalid version: JD-1 has revisions 1 to 1"
    );
    assert!(fx.store.get("JD-1", Some(0)).is_err());
    assert_eq!(
        fx.store.get("plan", None).unwrap_err().to_string(),
        "invalid document_id: \"plan\" is not a document ID like JD-42"
    );
}

#[test]
fn list_filters_sorts_and_pages() {
    let mut fx = fixture();
    for title in ["B", "A", "C"] {
        fx.store.create(&actor(), new_doc(title)).unwrap();
        fx.clock.advance(1);
    }
    let mut idea = new_doc("Idea");
    idea.doc_type = Some("ideas".into());
    idea.status = Some("done".into());
    fx.store.create(&actor(), idea).unwrap();

    let mut request = ListRequest {
        project: "joedocs".into(),
        doc_type: Some("plans".into()),
        status: None,
        sort: Sort::Title,
        start: 1,
        limit: 2,
    };
    let page = fx.store.list(&request).unwrap();
    let titles: Vec<_> = page.items.iter().map(|d| d.title.as_str()).collect();
    assert_eq!(titles, ["A", "B"]);
    assert_eq!((page.total, page.next_start), (3, Some(3)));
    request.start = 3;
    let page = fx.store.list(&request).unwrap();
    assert_eq!(page.items[0].title, "C");
    assert_eq!(page.next_start, None);

    request.doc_type = Some("all".into());
    request.start = 1;
    request.limit = 10;
    request.sort = Sort::UpdatedDesc;
    let page = fx.store.list(&request).unwrap();
    assert_eq!(page.items[0].id, "JD-4");
    request.status = Some("done".into());
    assert_eq!(fx.store.list(&request).unwrap().total, 1);

    for (edit, message) in [
        (
            Box::new(|r: &mut ListRequest| r.start = 0) as Box<dyn Fn(&mut ListRequest)>,
            "invalid start",
        ),
        (Box::new(|r| r.limit = 0), "invalid limit"),
        (Box::new(|r| r.limit = MAX_LIST_LIMIT + 1), "invalid limit"),
        (
            Box::new(|r| r.doc_type = Some("memo".into())),
            "invalid type",
        ),
        (
            Box::new(|r| r.status = Some("open".into())),
            "invalid status",
        ),
        (Box::new(|r| r.project = "nope".into()), "project nope"),
    ] {
        let mut bad = request.clone();
        edit(&mut bad);
        let err = fx.store.list(&bad).unwrap_err().to_string();
        assert!(err.starts_with(message), "{err}");
    }
}

#[test]
fn a_failed_history_commit_changes_nothing() {
    let mut fx = fixture();
    let created = fx.store.create(&actor(), new_doc("Plan")).unwrap();
    let before = read(&fx, &created.path);
    fx.fail.store(true, Ordering::SeqCst);

    let err = fx
        .store
        .update(&actor(), update("JD-1", "x", 1))
        .unwrap_err();
    assert_eq!(err.to_string(), "git commit failed: injected");
    assert_eq!(read(&fx, &created.path), before);
    assert!(fx.store.rename(&actor(), "JD-1", "Other", None).is_err());
    assert!(!fx.paths.home.join("joedocs/plans/Other.md").exists());
    assert_eq!(read(&fx, &created.path), before);
    assert!(fx.store.archive(&actor(), "JD-1", "x").is_err());
    assert_eq!(read(&fx, &created.path), before);
    assert!(fx.store.create(&actor(), new_doc("New")).is_err());
    assert!(!fx.paths.home.join("joedocs/plans/New.md").exists());

    fx.fail.store(false, Ordering::SeqCst);
    assert_eq!(fx.store.get("JD-1", None).unwrap().document.revision, 1);
    assert_eq!(
        fx.store.create(&actor(), new_doc("New")).unwrap().id,
        "JD-2"
    );
    assert_eq!(
        fx.store
            .update(&actor(), update("JD-1", "x", 1))
            .unwrap()
            .revision,
        2
    );
}

#[test]
fn writes_stop_when_the_history_moved_without_the_index() {
    let mut fx = fixture();
    fx.store.create(&actor(), new_doc("Plan")).unwrap();
    fs::write(fx.paths.home.join("stray.md"), "x").unwrap();
    git_for(&fx.paths)
        .commit(&Commit {
            paths: vec!["stray.md".into()],
            author: "someone".into(),
            at: jiff::Timestamp::UNIX_EPOCH,
            message: "outside".into(),
        })
        .unwrap();
    let err = fx
        .store
        .update(&actor(), update("JD-1", "x", 1))
        .unwrap_err();
    assert!(matches!(err, Error::OutOfSync { .. }), "{err}");
    assert!(err.to_string().contains("writes are refused"), "{err}");
}

#[test]
fn projects_are_validated_and_unique() {
    let mut fx = fixture();
    let project = fx.store.add_project("alpha", "AL", None).unwrap();
    assert_eq!(project.title, "alpha");
    let err = |result: Result<ProjectRow, Error>| result.unwrap_err().to_string();
    assert_eq!(
        err(fx.store.add_project("alpha", "AX", None)),
        "project alpha already exists"
    );
    assert_eq!(
        err(fx.store.add_project("beta", "AL", None)),
        "project key AL (used by alpha) already exists"
    );
    assert!(err(fx.store.add_project("Beta", "BE", None)).starts_with("invalid project"));
    assert!(err(fx.store.add_project("beta", "be", None)).starts_with("invalid key"));
    assert!(err(fx.store.add_project("beta", "BE", Some(""))).starts_with("invalid title"));
    let names: Vec<_> = fx
        .store
        .projects()
        .unwrap()
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert_eq!(names, ["alpha", "joedocs"]);
}

#[test]
fn keys_are_created_granted_and_revoked() {
    let mut fx = fixture();
    fx.store.add_project("alpha", "AL", None).unwrap();
    let grants = ["joedocs:write".parse().unwrap()];
    let created = fx
        .store
        .create_key("laptop", &grants, &CountingRandom(7))
        .unwrap();
    assert_eq!(created.key.id, "0708090a0b0c");
    let (id, secret) = crate::auth::parse_token(&created.token).unwrap();
    assert_eq!(id, created.key.id);
    assert!(!secret_is_empty(&secret));

    let err = fx
        .store
        .create_key("laptop", &[], &CountingRandom(1))
        .unwrap_err();
    assert_eq!(err.to_string(), "API key laptop already exists");
    let err = fx
        .store
        .create_key("desk", &["nope:read".parse().unwrap()], &CountingRandom(2))
        .unwrap_err();
    assert_eq!(err.to_string(), "project nope does not exist");
    assert_eq!(
        fx.store.keys().unwrap().len(),
        1,
        "failed creates leave nothing"
    );
    assert!(
        fx.store
            .create_key("Desk", &[], &CountingRandom(3))
            .unwrap_err()
            .to_string()
            .starts_with("invalid name")
    );

    let key = fx
        .store
        .grant("laptop", &["alpha:read".parse().unwrap()])
        .unwrap();
    assert_eq!(key.grants.len(), 2);
    assert_eq!(key.grants[0].permission, Permission::Read);
    assert!(fx.store.grant("laptop", &[]).is_err());
    assert!(matches!(
        fx.store.grant("nobody", &grants),
        Err(Error::KeyNotFound(_))
    ));

    let revoked = fx.store.revoke("laptop").unwrap();
    assert!(revoked.revoked_at.is_some());
    assert_eq!(
        fx.store.revoke("laptop").unwrap().revoked_at,
        revoked.revoked_at
    );
    assert!(
        fx.store
            .grant("laptop", &grants)
            .unwrap_err()
            .to_string()
            .contains("revoked")
    );
    assert!(matches!(
        fx.store.revoke("nobody"),
        Err(Error::KeyNotFound(_))
    ));
}

fn secret_is_empty(secret: &secrecy::SecretString) -> bool {
    use secrecy::ExposeSecret as _;
    secret.expose_secret().is_empty()
}

#[test]
fn actors_tidy_the_client_name() {
    let actor = Actor::new("laptop", "  Claude\tCode \n");
    assert_eq!((actor.name(), actor.client()), ("laptop", "Claude Code"));
    assert_eq!(Actor::new("x", &"c".repeat(100)).client().len(), 64);
}

#[test]
fn store_exposes_its_paths_and_settings() {
    let fx = fixture();
    assert_eq!(fx.store.paths(), &fx.paths);
    assert_eq!(fx.store.settings().default_type, "home");
    assert!(format!("{:?}", fx.store).starts_with("Store {"));
    assert!(fx.paths.lock_file().ends_with("state/joedocs.lock"));
}

#[test]
fn tokens_authenticate_until_revoked() {
    let mut fx = fixture();
    let created = fx
        .store
        .create_key(
            "laptop",
            &["joedocs:read".parse().unwrap()],
            &CountingRandom(3),
        )
        .unwrap();
    let key = fx.store.authenticate(&created.token).unwrap().unwrap();
    assert_eq!(key.name, "laptop");
    assert_eq!(
        fx.store.keys().unwrap()[0].last_used_at.as_deref(),
        Some("2026-10-26T07:33:20Z")
    );

    let token = secrecy::ExposeSecret::expose_secret(&created.token);
    let wrong_secret = format!(
        "{}{}",
        &token[..token.len() - 1],
        if token.ends_with('0') { "1" } else { "0" }
    );
    let unknown_id = format!("jd_ffffffffffff_{}", &token[16..]);
    for bad in [wrong_secret, unknown_id, "nonsense".to_owned()] {
        assert!(fx.store.authenticate(&bad.into()).unwrap().is_none());
    }
    fx.store.revoke("laptop").unwrap();
    assert!(fx.store.authenticate(&created.token).unwrap().is_none());
}

#[test]
fn documents_map_to_projects_by_key() {
    let fx = fixture();
    assert_eq!(
        fx.store.project_of_document("jd-99").unwrap().unwrap().name,
        "joedocs"
    );
    assert!(fx.store.project_of_document("XX-1").unwrap().is_none());
    assert!(fx.store.project_of_document("joedocs").is_err());
    assert_eq!(fx.store.project("joedocs").unwrap().unwrap().key, "JD");
    assert!(fx.store.project("JD").unwrap().is_none());
    fx.store
        .audit("k", &actor(), "archive_document", Some("JD-1"), "ok")
        .unwrap();
}
