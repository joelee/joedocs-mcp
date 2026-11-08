//! Store unit tests: a temporary vault, real git, and a history that can be
//! made to fail.

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

use super::*;
use crate::auth::{CountingRandom, OsRandom, Permission};
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
    fn diff(
        &self,
        from: (&str, &str),
        to: (&str, &str),
        context: u32,
    ) -> Result<String, HistoryError> {
        self.git.diff(from, to, context)
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

/// Every file under `dir`, vault-relative and sorted.
fn vault_files(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, prefix: &str, files: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
            if entry.file_type().unwrap().is_dir() {
                walk(&entry.path(), &format!("{name}/"), files);
            } else {
                files.push(name);
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, "", &mut files);
    files.sort();
    files
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
    // Only the project's generated INDEX.md, no document.
    assert_eq!(vault_files(&fx.paths.home), ["joedocs/INDEX.md"]);
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
        !fx.paths.home.join("joedocs/plans").exists(),
        "empty folders are removed"
    );
    assert_eq!(
        vault_files(&fx.paths.home),
        ["joedocs/INDEX.md"],
        "the project folder keeps only its INDEX.md"
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
fn rotating_a_key_replaces_its_token_but_keeps_its_id_and_grants() {
    let mut fx = fixture();
    let grants = ["joedocs:write".parse().unwrap()];
    let created = fx
        .store
        .create_key("laptop", &grants, &CountingRandom(3))
        .unwrap();
    let rotated = fx.store.rotate_key("laptop", &CountingRandom(90)).unwrap();
    assert_eq!(rotated.key.id, created.key.id);
    assert_eq!(rotated.key.grants, created.key.grants);
    let (id, _) = crate::auth::parse_token(&rotated.token).unwrap();
    assert_eq!(id, created.key.id);
    assert!(fx.store.authenticate(&created.token).unwrap().is_none());
    let key = fx.store.authenticate(&rotated.token).unwrap().unwrap();
    assert_eq!(key.name, "laptop");

    assert!(matches!(
        fx.store.rotate_key("nobody", &CountingRandom(1)),
        Err(Error::KeyNotFound(_))
    ));
    fx.store.revoke("laptop").unwrap();
    let err = fx
        .store
        .rotate_key("laptop", &CountingRandom(5))
        .unwrap_err();
    assert!(err.to_string().contains("is revoked"), "{err}");
    assert!(fx.store.authenticate(&rotated.token).unwrap().is_none());
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

/// The field and message of an `Error::Invalid`, or a description of what
/// came back instead.
fn invalid_parts<T: std::fmt::Debug>(result: Result<T, Error>) -> (String, String) {
    match result {
        Err(Error::Invalid { field, message }) => (field.to_owned(), message),
        other => (format!("not invalid: {other:?}"), String::new()),
    }
}

fn replacement(old: &str, new: &str) -> Replacement {
    Replacement {
        old: old.into(),
        new: new.into(),
    }
}

#[test]
fn changes_that_change_nothing_are_refused() {
    let mut fx = fixture();
    let mut request = new_doc("Plan");
    request.front_matter = fields(json!({"owner": "joel"}));
    let created = fx.store.create(&actor(), request).unwrap();
    let file = read(&fx, &created.path);
    let log = git_log(&fx);

    let edits = [
        vec![replacement("Body", "Body")],
        vec![replacement("Body", "Text"), replacement("Text", "Body")],
    ];
    for edits in edits {
        assert_eq!(
            invalid_parts(fx.store.edit(&actor(), "JD-1", &edits, Some(1), None)),
            ("edits".into(), "the edits leave the body unchanged".into())
        );
    }
    let same = [
        update("JD-1", "Body\n", 1),
        Update {
            front_matter: fields(json!({"owner": "joel", "status": "draft"})),
            ..update("JD-1", "Body\n", 1)
        },
    ];
    for change in same {
        assert_eq!(
            invalid_parts(fx.store.update(&actor(), change)),
            (
                "content".into(),
                "the content and front matter are unchanged".into()
            )
        );
    }
    let current = fields(json!({"owner": "joel", "status": "draft"}));
    assert_eq!(
        invalid_parts(
            fx.store
                .change_front_matter(&actor(), "JD-1", &current, &[], Some(1))
        ),
        (
            "front_matter".into(),
            "nothing would change; the keys already have these values".into()
        )
    );
    // Removing a key that is not set was already refused.
    let (field, message) = invalid_parts(fx.store.change_front_matter(
        &actor(),
        "JD-1",
        &Fields::new(),
        &["due".to_owned()],
        None,
    ));
    assert_eq!(field, "front_matter");
    assert!(message.contains("is not set"), "{message}");

    assert_eq!(fx.store.get("JD-1", None).unwrap().document.revision, 1);
    assert_eq!(fx.store.history("JD-1", 1, 10).unwrap().revisions.len(), 1);
    assert_eq!(read(&fx, &created.path), file);
    assert_eq!(git_log(&fx), log);
}

#[test]
fn changes_to_only_the_body_or_only_the_front_matter_still_succeed() {
    let mut fx = fixture();
    let mut request = new_doc("Plan");
    request.front_matter = fields(json!({"owner": "joel"}));
    fx.store.create(&actor(), request).unwrap();

    let body_only = Update {
        front_matter: fields(json!({"owner": "joel"})),
        ..update("JD-1", "New body\n", 1)
    };
    assert_eq!(fx.store.update(&actor(), body_only).unwrap().revision, 2);
    let front_matter_only = Update {
        front_matter: fields(json!({"owner": "ana"})),
        ..update("JD-1", "New body\n", 2)
    };
    assert_eq!(
        fx.store
            .update(&actor(), front_matter_only)
            .unwrap()
            .revision,
        3
    );
    let set = fields(json!({"owner": "ana", "status": "review"}));
    assert_eq!(
        fx.store
            .change_front_matter(&actor(), "JD-1", &set, &[], Some(3))
            .unwrap()
            .revision,
        4
    );
    let edits = [
        replacement("New", "Old"),
        replacement("Old body", "Final body"),
    ];
    assert_eq!(
        fx.store
            .edit(&actor(), "JD-1", &edits, Some(4), None)
            .unwrap()
            .revision,
        5
    );
}

#[test]
fn a_stale_revision_is_reported_before_a_no_op() {
    let mut fx = fixture();
    fx.store.create(&actor(), new_doc("Plan")).unwrap();
    fx.store
        .update(&actor(), update("JD-1", "Text\n", 1))
        .unwrap();
    let no_op = [replacement("Text", "Text")];
    assert!(matches!(
        fx.store.edit(&actor(), "JD-1", &no_op, Some(1), None),
        Err(Error::Conflict { current: 2, .. })
    ));
    assert!(matches!(
        fx.store.update(&actor(), update("JD-1", "Text\n", 1)),
        Err(Error::Conflict { current: 2, .. })
    ));
}

// --- INDEX.md --------------------------------------------------------------

/// What `<home>/joedocs/INDEX.md` should hold for the live documents.
fn expected_index(fx: &Fixture) -> String {
    let project = db::find_project(&fx.store.conn, "joedocs")
        .unwrap()
        .unwrap();
    let entries: Vec<index_file::Entry> = db::live_documents(&fx.store.conn)
        .unwrap()
        .iter()
        .filter(|row| row.project == "joedocs")
        .map(|row| index_file::Entry::from_row(&project, row))
        .collect();
    index_file::render(&project, &entries)
}

fn index_text(fx: &Fixture) -> Option<String> {
    fs::read_to_string(fx.paths.home.join("joedocs/INDEX.md")).ok()
}

#[test]
fn index_file_follows_every_kind_of_change() {
    let mut fx = fixture();
    // Adding the project wrote the empty index.
    assert!(
        index_text(&fx).unwrap().ends_with("No documents yet.\n"),
        "{:?}",
        index_text(&fx)
    );

    let created = fx.store.create(&actor(), new_doc("Plan")).unwrap();
    assert_eq!(index_text(&fx).unwrap(), expected_index(&fx));
    assert!(
        index_text(&fx)
            .unwrap()
            .contains("| JD-1 | [Plan](plans/Plan.md) |")
    );
    fx.store.create(&actor(), new_doc("Second")).unwrap();

    fx.clock.advance(86_400);
    fx.store
        .update(&actor(), update("JD-1", "New\n", created.revision))
        .unwrap();
    assert_eq!(index_text(&fx).unwrap(), expected_index(&fx));
    fx.store
        .edit(&actor(), "JD-1", &[replacement("New", "Newer")], None, None)
        .unwrap();
    fx.store
        .append(&actor(), "JD-1", "More.", None, None)
        .unwrap();
    fx.store
        .rename(&actor(), "JD-1", "Renamed plan", None)
        .unwrap();
    assert!(
        index_text(&fx)
            .unwrap()
            .contains("[Renamed plan](plans/Renamed%20plan.md)")
    );
    fx.store
        .move_document(&actor(), "JD-1", "ideas", None)
        .unwrap();
    assert!(
        index_text(&fx)
            .unwrap()
            .contains("](ideas/Renamed%20plan.md) | ideas |")
    );
    let status = fields(json!({"status": "review"}));
    fx.store
        .change_front_matter(&actor(), "JD-1", &status, &[], None)
        .unwrap();
    assert!(index_text(&fx).unwrap().contains("| ideas | review |"));
    fx.store
        .restore_version(&actor(), "JD-1", 1, None, None)
        .unwrap();
    assert_eq!(index_text(&fx).unwrap(), expected_index(&fx));

    fx.store.archive(&actor(), "JD-2", "done with it").unwrap();
    assert!(!index_text(&fx).unwrap().contains("JD-2"));
    fx.store.unarchive(&actor(), "JD-2").unwrap();
    assert!(index_text(&fx).unwrap().contains("| JD-2 |"));
    assert_eq!(index_text(&fx).unwrap(), expected_index(&fx));

    // INDEX.md is never part of a commit.
    let output = Command::new("git")
        .arg("--git-dir")
        .arg(fx.paths.git_dir())
        .args(["log", "--name-only", "--format="])
        .output()
        .unwrap();
    let names = String::from_utf8(output.stdout).unwrap();
    assert!(!names.contains("INDEX.md"), "{names}");
}

#[test]
fn an_unchanged_index_file_is_not_rewritten() {
    let mut fx = fixture();
    fx.store.create(&actor(), new_doc("Plan")).unwrap();
    let path = fx.paths.home.join("joedocs/INDEX.md");
    let long_ago = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(long_ago)
        .unwrap();
    // Same day, same title, type and status: the table does not change.
    fx.store
        .update(&actor(), update("JD-1", "Other body\n", 1))
        .unwrap();
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), long_ago);
    // A new title does change it.
    fx.store
        .rename(&actor(), "JD-1", "New title", None)
        .unwrap();
    assert_ne!(fs::metadata(&path).unwrap().modified().unwrap(), long_ago);
}

/// A store over `fx`'s vault and history with `index_file` set to `on`.
fn reopen_with_index_file(fx: &Fixture, on: bool) -> Store {
    let settings = Settings {
        index_file: on,
        ..Settings::from_config(&Config::default())
    };
    Store::open(
        fx.paths.clone(),
        settings,
        Box::new(git_for(&fx.paths)),
        Arc::clone(&fx.clock) as Arc<dyn Clock>,
    )
    .unwrap()
}

#[test]
fn with_index_file_off_no_index_is_written_and_old_ones_stay() {
    let fx = fixture();
    let mut store = reopen_with_index_file(&fx, false);
    store.add_project("other", "OT", None).unwrap();
    assert!(!fx.paths.home.join("other/INDEX.md").exists());
    let before = index_text(&fx).unwrap();
    store.create(&actor(), new_doc("Plan")).unwrap();
    assert_eq!(index_text(&fx).unwrap(), before);
}

/// Everything `tracing` writes while `f` runs, as text.
fn captured_logs(f: impl FnOnce()) -> String {
    #[derive(Clone, Default)]
    struct Buffer(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buffer = Buffer::default();
    let writer = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, f);
    let bytes = buffer.0.lock().unwrap().clone();
    String::from_utf8(bytes).unwrap()
}

#[test]
fn a_failed_index_write_warns_and_keeps_the_change() {
    let mut fx = fixture();
    let path = fx.paths.home.join("joedocs/INDEX.md");
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    let mut changed = None;
    let logs = captured_logs(|| changed = Some(fx.store.create(&actor(), new_doc("Plan"))));
    let changed = changed.unwrap().unwrap();
    assert_eq!(changed.revision, 1);
    assert!(fx.paths.home.join(&changed.path).is_file());
    assert!(git_log(&fx).contains("Create JD-1"), "{}", git_log(&fx));
    assert!(logs.contains("WARN"), "{logs}");
    assert!(logs.contains("INDEX.md"), "{logs}");
}

#[test]
fn the_history_ignores_index_files_from_init_and_on_open() {
    let mut fx = fixture();
    let exclude = fx.paths.git_dir().join("info/exclude");
    let lines = |text: &str| text.lines().filter(|l| *l == "/*/INDEX.md").count();
    assert_eq!(lines(&fs::read_to_string(&exclude).unwrap()), 1);

    fx.store.create(&actor(), new_doc("Plan")).unwrap();
    let status = Command::new("git")
        .arg("--git-dir")
        .arg(fx.paths.git_dir())
        .arg("--work-tree")
        .arg(&fx.paths.home)
        .args(["status", "--porcelain", "--untracked-files=all"])
        .output()
        .unwrap();
    let status = String::from_utf8(status.stdout).unwrap();
    assert!(!status.contains("INDEX.md"), "{status}");

    // An older install: the line is missing, other lines are kept.
    fs::write(&exclude, "# local rules\n*.swp").unwrap();
    drop(reopen_with_index_file(&fx, true));
    let text = fs::read_to_string(&exclude).unwrap();
    assert!(text.starts_with("# local rules\n*.swp\n"), "{text}");
    assert_eq!(lines(&text), 1, "{text}");
    drop(reopen_with_index_file(&fx, true));
    assert_eq!(fs::read_to_string(&exclude).unwrap(), text, "added once");
}

#[test]
fn reindex_and_refresh_rewrite_every_index_file() {
    let mut fx = fixture();
    fx.store.create(&actor(), new_doc("Plan")).unwrap();
    let path = fx.paths.home.join("joedocs/INDEX.md");
    fs::remove_file(&path).unwrap();
    fx.store.reindex().unwrap();
    assert_eq!(index_text(&fx).unwrap(), expected_index(&fx));

    fs::write(&path, "edited by hand\n").unwrap();
    fx.store.refresh_index_files();
    assert_eq!(index_text(&fx).unwrap(), expected_index(&fx));
}

#[test]
fn check_reports_a_missing_or_stale_index_file() {
    let mut fx = fixture();
    fx.store.create(&actor(), new_doc("Plan")).unwrap();
    assert_eq!(fx.store.check().unwrap().problems(), 0);

    let path = fx.paths.home.join("joedocs/INDEX.md");
    fs::write(&path, "edited by hand\n").unwrap();
    let report = fx.store.check().unwrap();
    assert_eq!(report.stale_index_files, ["joedocs/INDEX.md"]);
    assert!(report.unknown_files.is_empty(), "{report:?}");
    assert_eq!(report.problems(), 1);

    fs::remove_file(&path).unwrap();
    assert_eq!(
        fx.store.check().unwrap().stale_index_files,
        ["joedocs/INDEX.md"]
    );

    // With index_file off, INDEX.md is neither expected nor unknown.
    let off = reopen_with_index_file(&fx, false);
    assert_eq!(off.check().unwrap().problems(), 0);
    fs::write(&path, "left over\n").unwrap();
    assert_eq!(off.check().unwrap().problems(), 0);
}

#[test]
fn check_reports_an_index_file_outside_a_project_as_unknown() {
    let fx = fixture();
    fs::create_dir_all(fx.paths.home.join("stray")).unwrap();
    fs::write(fx.paths.home.join("stray/INDEX.md"), "not a project\n").unwrap();
    let report = fx.store.check().unwrap();
    assert_eq!(report.unknown_files, ["stray/INDEX.md"]);
    assert!(report.stale_index_files.is_empty(), "{report:?}");
}

// --- Admin tokens -----------------------------------------------------------

/// The current TOTP code of `created`, `offset` steps from the clock's.
fn admin_code(fx: &Fixture, created: &CreatedAdminToken, offset: i64) -> String {
    use secrecy::ExposeSecret as _;
    let secret = crate::totp::base32_decode(created.totp_secret.expose_secret()).unwrap();
    let step = crate::totp::step_at(fx.clock.now().as_second()) + offset;
    crate::totp::code(&secret, step)
}

#[test]
fn admin_login_accepts_one_step_of_skew_and_each_step_once() {
    let mut fx = fixture();
    let random = CountingRandom(40);
    let created = fx.store.create_admin_token("laptop", &random).unwrap();
    assert!(matches!(
        fx.store.create_admin_token("laptop", &random),
        Err(Error::AlreadyExists(_))
    ));
    for offset in [-1, 0, 1] {
        // A fresh minute for each, so the steps do not overlap.
        fx.clock.advance(120);
        let code = admin_code(&fx, &created, offset);
        let session = fx
            .store
            .admin_login(&created.token, &code, 8, &OsRandom)
            .unwrap();
        assert!(session.expires_at > fx.store.now());
        // The same code (the same step) is refused the second time.
        let again = fx.store.admin_login(&created.token, &code, 8, &OsRandom);
        assert!(matches!(
            again,
            Err(Error::AdminRefused(AdminRefusal::Unauthorized))
        ));
    }
    for offset in [-2, 2] {
        fx.clock.advance(120);
        let code = admin_code(&fx, &created, offset);
        assert!(
            fx.store
                .admin_login(&created.token, &code, 8, &OsRandom)
                .is_err()
        );
    }
}

#[test]
fn five_failed_logins_lock_the_token_for_fifteen_minutes() {
    let mut fx = fixture();
    let random = CountingRandom(40);
    let created = fx.store.create_admin_token("laptop", &random).unwrap();
    // Four failures, then a success, reset the count.
    for _ in 0..4 {
        assert!(
            fx.store
                .admin_login(&created.token, "000000", 8, &OsRandom)
                .is_err()
        );
    }
    let code = admin_code(&fx, &created, 0);
    fx.store
        .admin_login(&created.token, &code, 8, &OsRandom)
        .unwrap();
    assert_eq!(fx.store.admin_tokens().unwrap()[0].failed_logins, 0);
    // Five in a row lock it, even for the right code.
    for _ in 0..MAX_FAILED_LOGINS {
        let refused = fx.store.admin_login(&created.token, "000000", 8, &OsRandom);
        assert!(matches!(
            refused,
            Err(Error::AdminRefused(AdminRefusal::Unauthorized))
        ));
    }
    fx.clock.advance(60);
    let code = admin_code(&fx, &created, 0);
    let locked = fx.store.admin_login(&created.token, &code, 8, &OsRandom);
    match locked {
        Err(Error::AdminRefused(AdminRefusal::Locked { retry_after, .. })) => {
            assert!(retry_after > 0 && retry_after <= 900, "{retry_after}");
        }
        other => panic!("expected a lock, got {other:?}"),
    }
    fx.clock.advance(LOCK_SECONDS);
    let code = admin_code(&fx, &created, 0);
    fx.store
        .admin_login(&created.token, &code, 8, &OsRandom)
        .unwrap();
}

#[test]
fn sessions_expire_belong_to_their_token_and_end_with_it() {
    let mut fx = fixture();
    let random = CountingRandom(40);
    let a = fx.store.create_admin_token("a", &random).unwrap();
    let b = fx
        .store
        .create_admin_token("b", &CountingRandom(90))
        .unwrap();
    let code = admin_code(&fx, &a, 0);
    let session = fx.store.admin_login(&a.token, &code, 8, &OsRandom).unwrap();
    assert_eq!(
        fx.store
            .admin_session(&a.token, &session.token)
            .unwrap()
            .name,
        "a"
    );
    assert!(matches!(
        fx.store.admin_session(&b.token, &session.token),
        Err(Error::AdminRefused(AdminRefusal::NoSession))
    ));
    let bogus = secrecy::SecretString::from("jda_000000000000_".to_owned() + &"0".repeat(64));
    assert!(matches!(
        fx.store.admin_session(&bogus, &session.token),
        Err(Error::AdminRefused(AdminRefusal::Unauthorized))
    ));
    fx.clock.advance(8 * 3600);
    assert!(matches!(
        fx.store.admin_session(&a.token, &session.token),
        Err(Error::AdminRefused(AdminRefusal::SessionExpired))
    ));

    let code = admin_code(&fx, &a, 0);
    let second = fx.store.admin_login(&a.token, &code, 8, &OsRandom).unwrap();
    fx.store.end_admin_session(&a.token, &second.token).unwrap();
    assert!(matches!(
        fx.store.admin_session(&a.token, &second.token),
        Err(Error::AdminRefused(AdminRefusal::NoSession))
    ));

    fx.clock.advance(60);
    let code = admin_code(&fx, &a, 0);
    let third = fx.store.admin_login(&a.token, &code, 8, &OsRandom).unwrap();
    let revoked = fx.store.revoke_admin_token("a").unwrap();
    assert!(revoked.revoked_at.is_some());
    assert!(fx.store.admin_session(&a.token, &third.token).is_err());
    assert!(fx.store.admin_token_for(&a.token).unwrap().is_none());
    assert!(matches!(
        fx.store.revoke_admin_token("nobody"),
        Err(Error::NotFound(_))
    ));
}

// --- State permissions (JD-10 STEP-01) ----------------------------------------

fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn a_new_store_keeps_its_state_private() {
    let fx = fixture();
    assert_eq!(mode_of(&fx.paths.state_dir), 0o700);
    for name in ["joedocs.db", "joedocs.db-wal", "joedocs.db-shm"] {
        let path = fx.paths.state_dir.join(name);
        if path.exists() {
            assert_eq!(mode_of(&path), 0o600, "{name}");
        }
    }
}

#[test]
fn opening_an_existing_store_tightens_readable_state() {
    use std::os::unix::fs::PermissionsExt as _;
    let fx = fixture();
    fs::set_permissions(&fx.paths.state_dir, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(fx.paths.db(), fs::Permissions::from_mode(0o644)).unwrap();
    let logs = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let sink = std::sync::Arc::clone(&logs);
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || Sink(std::sync::Arc::clone(&sink)))
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let reopened = Store::open(
            fx.paths.clone(),
            Settings::from_config(&Config::default()),
            Box::new(git_for(&fx.paths)),
            Arc::new(FixedClock::at(1_793_000_000)),
        );
        assert!(reopened.is_ok());
    });
    assert_eq!(mode_of(&fx.paths.state_dir), 0o700);
    assert_eq!(mode_of(&fx.paths.db()), 0o600);
    let text = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
    assert!(text.contains("made") && text.contains("private"), "{text}");
}

/// A log writer into a shared buffer.
struct Sink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Ok(mut logs) = self.0.lock() {
            logs.extend_from_slice(buf);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_failed_history_commit_leaves_no_partial_folder() {
    let mut fx = fixture();
    let mut doc = new_doc("Roadmap");
    doc.multifile = true;
    fx.store.create(&actor(), doc).unwrap();
    let request = |file: &str| NewFile {
        document_id: "JD-1".into(),
        file: file.into(),
        content: "x\n".into(),
        ..NewFile::default()
    };
    fx.store.create_file(&actor(), &request("a")).unwrap();
    let files_before = vault_files(&fx.paths.home);
    let a_before = read(&fx, "joedocs/plans/Roadmap/a.md");
    fx.fail.store(true, Ordering::SeqCst);

    assert!(fx.store.create_file(&actor(), &request("new/b")).is_err());
    assert!(
        fx.store
            .append_file(&actor(), "JD-1", "a", "more\n", None, None)
            .is_err()
    );
    assert!(
        fx.store
            .rename_file(&actor(), "JD-1", "a", "moved/a", None)
            .is_err()
    );
    let writes = WriteFiles {
        document_id: "JD-1".into(),
        files: vec![
            FileWrite {
                file: "a".into(),
                content: "y\n".into(),
                front_matter: Fields::new(),
            },
            FileWrite {
                file: "c/d".into(),
                content: "z\n".into(),
                front_matter: fields(json!({"k": 1})),
            },
        ],
        expected_revision: None,
        summary: None,
    };
    assert!(fx.store.write_files(&actor(), &writes).is_err());
    let mut files_after = vault_files(&fx.paths.home);
    files_after.retain(|f| !f.ends_with("INDEX.md"));
    let mut expected = files_before.clone();
    expected.retain(|f| !f.ends_with("INDEX.md"));
    assert_eq!(files_after, expected);
    assert_eq!(read(&fx, "joedocs/plans/Roadmap/a.md"), a_before);

    fx.fail.store(false, Ordering::SeqCst);
    assert_eq!(fx.store.get("JD-1", None).unwrap().document.revision, 2);
    let listed: Vec<String> = fx
        .store
        .list_files("JD-1", None)
        .unwrap()
        .into_iter()
        .map(|f| f.file)
        .collect();
    assert_eq!(listed, ["Roadmap", "a"]);
    assert_eq!(fx.store.write_files(&actor(), &writes).unwrap().revision, 3);
    assert_eq!(fx.store.check().unwrap().problems(), 0);
}
