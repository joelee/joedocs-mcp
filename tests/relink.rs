//! `Store::rename_with` and `rewrite_links`: wiki links to a renamed
//! document rewritten in the other documents of its project, in one commit.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use joedocs::Config;
use joedocs::clock::FixedClock;
use joedocs::history::{GitCli, History as _, Logged};
use joedocs::store::{
    Actor, Error, NewDocument, NewFile, Paths, Relinked, RenameOptions, Settings, Store,
};
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

fn open_with(root: &Path, config: &Config) -> Result<Store, Error> {
    let paths = paths(root);
    Store::init(&paths, &git(root))?;
    let mut store = Store::open(
        paths,
        Settings::from_config(config),
        Box::new(git(root)),
        Arc::new(FixedClock::at(1_793_000_000)),
    )?;
    if store.projects()?.is_empty() {
        store.add_project("joedocs", "JD", None)?;
        store.add_project("infra", "IN", None)?;
    }
    Ok(store)
}

fn open(root: &Path) -> Result<Store, Error> {
    open_with(root, &Config::default())
}

fn actor() -> Actor {
    Actor::new("laptop", "claude-code")
}

fn create(store: &mut Store, project: &str, title: &str, body: &str) -> Result<String, Error> {
    create_typed(store, project, "plans", title, body, false)
}

fn create_typed(
    store: &mut Store,
    project: &str,
    doc_type: &str,
    title: &str,
    body: &str,
    multifile: bool,
) -> Result<String, Error> {
    Ok(store
        .create(
            &actor(),
            NewDocument {
                project: project.into(),
                doc_type: Some(doc_type.into()),
                title: title.into(),
                content: body.into(),
                multifile,
                ..NewDocument::default()
            },
        )?
        .id)
}

fn body(store: &Store, id: &str) -> TestResult<String> {
    let markdown = store.get(id, None)?.markdown;
    let (_, body) = markdown.split_once("\n---\n").ok_or("no front matter")?;
    Ok(body.to_owned())
}

fn file_body(store: &Store, id: &str, file: &str) -> TestResult<String> {
    let markdown = store.get_file(id, file, None)?.markdown;
    let (_, body) = markdown.split_once("\n---\n").ok_or("no front matter")?;
    Ok(body.to_owned())
}

fn relinking() -> RenameOptions {
    RenameOptions {
        rewrite_links: true,
    }
}

fn last_commit(root: &Path) -> TestResult<Logged> {
    Ok(git(root).log()?.pop().ok_or("no commits")?)
}

fn trailers(commit: &Logged, key: &str) -> Vec<String> {
    commit
        .trailers
        .iter()
        .filter(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
        .collect()
}

const LINKS: &str = "See [[Old plan]], [[old plan|the plan]], [[Old plan#Scope]],\n\
                     [[Old plan#Scope|scope]] and ![[Old plan]].\n\n\
                     Not `[[Old plan]]`, nor\n\n```\n[[Old plan]]\n```\n\n\
                     and [[Other]] stays.\n";

const RELINKED: &str = "See [[New plan]], [[New plan|the plan]], [[New plan#Scope]],\n\
                        [[New plan#Scope|scope]] and ![[New plan]].\n\n\
                        Not `[[Old plan]]`, nor\n\n```\n[[Old plan]]\n```\n\n\
                        and [[Other]] stays.\n";

#[test]
fn every_link_form_is_rewritten_in_one_commit() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    let plan = create(
        &mut store,
        "joedocs",
        "Old plan",
        "Links to [[Old plan]] itself.\n",
    )
    .unwrap();
    let notes = create(&mut store, "joedocs", "Notes", LINKS).unwrap();
    let unrelated = create(&mut store, "joedocs", "Other", "No links.\n").unwrap();
    let commits = git(dir.path()).log().unwrap().len();

    let renamed = store
        .rename_with(&actor(), &plan, "New plan", Some(1), relinking())
        .unwrap();
    assert_eq!(renamed.changed.revision, 2);
    assert_eq!(
        renamed.relinked,
        [Relinked {
            document_id: notes.clone(),
            revision: 2
        }]
    );
    assert_eq!(body(&store, &notes).unwrap(), RELINKED);
    // The renamed document's own body is not rewritten, and keeps the alias.
    assert_eq!(
        body(&store, &plan).unwrap(),
        "Links to [[Old plan]] itself.\n"
    );
    assert!(
        store
            .get(&plan, None)
            .unwrap()
            .markdown
            .contains("- Old plan\n")
    );
    assert_eq!(store.get(&unrelated, None).unwrap().document.revision, 1);

    let log = git(dir.path()).log().unwrap();
    assert_eq!(log.len(), commits + 1, "one commit");
    let commit = log.last().unwrap();
    assert_eq!(commit.trailer("JoeDocs-Document"), Some(plan.as_str()));
    assert_eq!(commit.trailer("JoeDocs-Action"), Some("rename"));
    assert_eq!(trailers(commit, "JoeDocs-Relinked"), [format!("{notes} 2")]);
    assert_eq!(
        trailers(commit, "JoeDocs-Relinked-File"),
        Vec::<String>::new()
    );

    let history = store.history(&notes, 1, 10).unwrap();
    let latest = &history.revisions[0];
    assert_eq!((latest.revision, latest.action.as_str()), (2, "relink"));
    assert_eq!(latest.summary, format!("Rewrite links to renamed {plan}"));
    assert_eq!(latest.commit, commit.id);
    assert_eq!(store.check().unwrap().problems(), 0);
}

#[test]
fn only_live_documents_of_the_project_that_resolve_to_it_are_rewritten() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    let plan = create(&mut store, "joedocs", "Old plan", "Body.\n").unwrap();
    // A note with the same title, newer, so links resolve to the plan.
    let twin = create_typed(&mut store, "joedocs", "notes", "Old plan", "Twin.\n", false).unwrap();
    let other_project = create(&mut store, "infra", "Infra", "[[Old plan]]\n").unwrap();
    let archived = create(&mut store, "joedocs", "Gone", "[[Old plan]]\n").unwrap();
    store.archive(&actor(), &archived, "old").unwrap();
    let live = create(&mut store, "joedocs", "Live", "[[Old plan]]\n").unwrap();

    // Renaming the twin rewrites nothing: its title's links go to the plan.
    let renamed = store
        .rename_with(&actor(), &twin, "Twin note", None, relinking())
        .unwrap();
    assert_eq!(renamed.relinked, []);
    assert_eq!(body(&store, &live).unwrap(), "[[Old plan]]\n");

    let renamed = store
        .rename_with(&actor(), &plan, "New plan", None, relinking())
        .unwrap();
    assert_eq!(renamed.relinked.len(), 1);
    assert_eq!(renamed.relinked[0].document_id, live);
    assert_eq!(body(&store, &live).unwrap(), "[[New plan]]\n");
    assert_eq!(body(&store, &other_project).unwrap(), "[[Old plan]]\n");
    assert_eq!(
        store.get(&other_project, None).unwrap().document.revision,
        1
    );
    let commit = last_commit(dir.path()).unwrap();
    assert_eq!(trailers(&commit, "JoeDocs-Relinked"), [format!("{live} 2")]);
    assert_eq!(store.check().unwrap().problems(), 0);
}

#[test]
fn files_of_a_multi_file_document_are_rewritten_and_named() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    let plan = create(&mut store, "joedocs", "Old plan", "Body.\n").unwrap();
    let folder = create_typed(
        &mut store,
        "joedocs",
        "notes",
        "Folder",
        "Main [[Old plan]].\n",
        true,
    )
    .unwrap();
    for (file, content) in [
        ("items/one", "One [[Old plan|plan]].\n"),
        ("items/two", "Two, no link.\n"),
    ] {
        store
            .create_file(
                &actor(),
                &NewFile {
                    document_id: folder.clone(),
                    file: file.into(),
                    content: content.into(),
                    ..NewFile::default()
                },
            )
            .unwrap();
    }

    let renamed = store
        .rename_with(&actor(), &plan, "New plan", None, relinking())
        .unwrap();
    assert_eq!(
        renamed.relinked,
        [Relinked {
            document_id: folder.clone(),
            revision: 4
        }]
    );
    assert_eq!(body(&store, &folder).unwrap(), "Main [[New plan]].\n");
    assert_eq!(
        file_body(&store, &folder, "items/one").unwrap(),
        "One [[New plan|plan]].\n"
    );
    let files = store.list_files(&folder, None).unwrap();
    let two = files.iter().find(|f| f.file == "items/two").unwrap();
    assert_eq!(two.revision, 3, "a file without links is not rewritten");

    let commit = last_commit(dir.path()).unwrap();
    assert_eq!(
        trailers(&commit, "JoeDocs-Relinked"),
        [format!("{folder} 4")]
    );
    assert_eq!(
        trailers(&commit, "JoeDocs-Relinked-File"),
        [
            format!("{folder} Folder.md"),
            format!("{folder} items/one.md")
        ]
    );
    assert_eq!(store.check().unwrap().problems(), 0);
}

#[test]
fn without_the_option_no_link_is_rewritten() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    let plan = create(&mut store, "joedocs", "Old plan", "Body.\n").unwrap();
    let notes = create(&mut store, "joedocs", "Notes", LINKS).unwrap();
    let renamed = store
        .rename_with(&actor(), &plan, "New plan", None, RenameOptions::default())
        .unwrap();
    assert_eq!(renamed.relinked, []);
    assert_eq!(body(&store, &notes).unwrap(), LINKS);
    let commit = last_commit(dir.path()).unwrap();
    assert_eq!(trailers(&commit, "JoeDocs-Relinked"), Vec::<String>::new());
    // `rename` is the same call without the option.
    let changed = store
        .rename(&actor(), &plan, "Newer plan", Some(2))
        .unwrap();
    assert_eq!(changed.revision, 3);
    assert_eq!(body(&store, &notes).unwrap(), LINKS);
}

/// Every vault file and the index's view of each document, to show that
/// a refused rename changed nothing.
fn state(root: &Path, store: &Store, ids: &[&str]) -> TestResult<Vec<String>> {
    let mut lines = Vec::new();
    let mut stack = vec![paths(root).home];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else {
                lines.push(format!("{} {}", path.display(), fs::read_to_string(&path)?));
            }
        }
    }
    lines.sort();
    for id in ids {
        let info = store.history(id, 1, 1)?.document;
        lines.push(format!("{id} r{} {}", info.revision, info.title));
    }
    lines.push(format!("commits {}", git(root).log()?.len()));
    Ok(lines)
}

#[test]
fn an_oversize_rewrite_is_refused_and_changes_nothing() {
    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.max_document_bytes = 420;
    let mut store = open_with(dir.path(), &config).unwrap();
    let plan = create(&mut store, "joedocs", "P", "Body.\n").unwrap();
    let small = create(&mut store, "joedocs", "Small", "[[P]]\n").unwrap();
    let big = create(&mut store, "joedocs", "Big", &"[[P]] ".repeat(30)).unwrap();
    let before = state(dir.path(), &store, &[&plan, &small, &big]).unwrap();

    let err = store
        .rename_with(&actor(), &plan, "A much longer title", None, relinking())
        .unwrap_err();
    assert!(
        matches!(
            err,
            Error::Invalid {
                field: "rewrite_links",
                ..
            }
        ),
        "{err:?}"
    );
    assert!(err.to_string().contains(&big), "{err}");
    assert_eq!(
        state(dir.path(), &store, &[&plan, &small, &big]).unwrap(),
        before
    );
    assert_eq!(store.check().unwrap().problems(), 0);
}

#[test]
fn a_hand_edited_linking_file_is_refused_and_changes_nothing() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    let plan = create(&mut store, "joedocs", "Old plan", "Body.\n").unwrap();
    let notes = create(&mut store, "joedocs", "Notes", "[[Old plan]]\n").unwrap();
    let path = paths(dir.path())
        .home
        .join(store.get(&notes, None).unwrap().document.path);
    let edited = format!("{}hand edit\n", fs::read_to_string(&path).unwrap());
    fs::write(&path, &edited).unwrap();
    let before = state(dir.path(), &store, &[&plan, &notes]).unwrap();

    let err = store
        .rename_with(&actor(), &plan, "New plan", None, relinking())
        .unwrap_err();
    assert!(
        matches!(err, Error::Integrity { ref id, .. } if *id == notes),
        "{err:?}"
    );
    assert_eq!(state(dir.path(), &store, &[&plan, &notes]).unwrap(), before);
}

#[test]
fn a_title_without_words_is_found_without_the_search_index() {
    let dir = TempDir::new().unwrap();
    let mut store = open(dir.path()).unwrap();
    let odd = create(&mut store, "joedocs", "+++", "Body.\n").unwrap();
    let notes = create(&mut store, "joedocs", "Notes", "[[+++]] and [[+++|plus]]\n").unwrap();
    let renamed = store
        .rename_with(&actor(), &odd, "Plus", None, relinking())
        .unwrap();
    assert_eq!(renamed.relinked.len(), 1);
    assert_eq!(
        body(&store, &notes).unwrap(),
        "[[Plus]] and [[Plus|plus]]\n"
    );
    assert_eq!(store.check().unwrap().problems(), 0);
}
