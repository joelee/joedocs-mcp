//! Search over a fixture vault: two projects with documents of several
//! types, tags, statuses and custom keys, one archived, written through the
//! store against real git.

use std::path::Path;
use std::sync::Arc;

use joedocs::Config;
use joedocs::clock::FixedClock;
use joedocs::document::front_matter::Fields;
use joedocs::history::GitCli;
use joedocs::store::{
    Actor, Error, NewDocument, Paths, SearchRequest, SearchResults, Settings, Store, Update,
};
use serde_json::{Map, Value, json};
use tempfile::TempDir;

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

struct Vault {
    _dir: TempDir,
    store: Store,
}

fn fields(value: &Value) -> Fields {
    value
        .as_object()
        .map(|map| map.clone().into_iter().collect())
        .unwrap_or_default()
}

fn open(root: &Path, clock: Arc<FixedClock>) -> Result<Store, Error> {
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
        clock,
    )
}

/// `joedocs` (JD-1 to JD-4, JD-4 archived) and `infra` (IN-1), created a
/// minute apart in that order.
fn vault() -> TestResult<Vault> {
    let dir = TempDir::new()?;
    let clock = Arc::new(FixedClock::at(1_793_000_000));
    let mut store = open(dir.path(), Arc::clone(&clock))?;
    store.add_project("joedocs", "JD", Some("JoeDocs"))?;
    store.add_project("infra", "IN", Some("Infrastructure"))?;
    let actor = Actor::new("laptop", "test");
    let docs = [
        (
            "joedocs",
            "plans",
            "approved",
            "Rate limiting plan",
            "We retry with exponential backoff when the rate limit is hit.\n\
             Redis is not involved.\n",
            json!({"tags": ["security", "api"], "owner": "joel", "priority": 2}),
        ),
        (
            "joedocs",
            "ideas",
            "draft",
            "Search grammar",
            "Bare words, quoted phrases and prefix matching. Needs a review.\n",
            json!({"tags": ["search"], "priority": 5}),
        ),
        (
            "joedocs",
            "reviews",
            "review",
            "Auth review",
            "The authentication flow retries expired tokens. Sessions live in Redis.\n",
            json!({"tags": ["security"], "approved_by": "joel"}),
        ),
        (
            "joedocs",
            "notes",
            "draft",
            "Old note",
            "Rate limit leftovers.\n",
            json!({}),
        ),
        (
            "infra",
            "plans",
            "approved",
            "Cache rollout",
            "Valkey replaces Redis as the cache.\n",
            json!({"tags": ["cache"]}),
        ),
    ];
    for (project, doc_type, status, title, content, extra) in docs {
        store.create(
            &actor,
            NewDocument {
                project: project.into(),
                doc_type: Some(doc_type.into()),
                status: Some(status.into()),
                title: title.into(),
                content: content.into(),
                front_matter: fields(&extra),
                summary: None,
            },
        )?;
        clock.advance(60);
    }
    store.archive(&actor, "JD-4", "obsolete")?;
    Ok(Vault { _dir: dir, store })
}

fn request(projects: &[&str], query: &str, filters: &Value) -> SearchRequest {
    SearchRequest {
        projects: projects.iter().map(|p| (*p).to_owned()).collect(),
        doc_type: None,
        query: query.to_owned(),
        filters: filters.as_object().cloned().unwrap_or_else(Map::new),
        start: 1,
        limit: 20,
    }
}

fn ids(results: &SearchResults) -> Vec<&str> {
    results
        .items
        .iter()
        .map(|hit| hit.document.id.as_str())
        .collect()
}

const BOTH: &[&str] = &["joedocs", "infra"];
const NONE: [&str; 0] = [];

fn find(vault: &Vault, projects: &[&str], query: &str) -> TestResult<Vec<String>> {
    let results = vault
        .store
        .search(&request(projects, query, &Value::Null))?;
    Ok(ids(&results).into_iter().map(str::to_owned).collect())
}

fn filter(vault: &Vault, projects: &[&str], filters: &Value) -> TestResult<Vec<String>> {
    let results = vault.store.search(&request(projects, "", filters))?;
    Ok(ids(&results).into_iter().map(str::to_owned).collect())
}

#[test]
fn every_word_must_match_and_archived_documents_are_left_out() {
    let vault = vault().unwrap();
    let results = vault
        .store
        .search(&request(&["joedocs"], "rate limit", &Value::Null))
        .unwrap();
    assert_eq!(ids(&results), ["JD-1"]);
    assert_eq!(results.total, 1);
    let hit = &results.items[0];
    assert!(hit.score.unwrap() > 0.0);
    let snippet = hit.snippet.as_deref().unwrap();
    assert!(snippet.contains("«rate» «limit»"), "{snippet}");
    assert_eq!(hit.document.title, "Rate limiting plan");
    assert_eq!(hit.document.doc_type, "plans");
    assert_eq!(hit.document.status, "approved");
}

#[test]
fn the_grammar_supports_phrases_exclusions_or_and_prefixes() {
    let vault = vault().unwrap();
    assert_eq!(find(&vault, BOTH, "\"rate limit\"").unwrap(), ["JD-1"]);
    assert_eq!(find(&vault, BOTH, "\"limit rate\"").unwrap(), NONE);
    let mut redis = find(&vault, BOTH, "redis -valkey").unwrap();
    redis.sort();
    assert_eq!(redis, ["JD-1", "JD-3"]);
    let mut either = find(&vault, BOTH, "valkey OR grammar").unwrap();
    either.sort();
    assert_eq!(either, ["IN-1", "JD-2"]);
    assert_eq!(find(&vault, BOTH, "authent*").unwrap(), ["JD-3"]);
    // Unbalanced quotes and FTS5 syntax are just words.
    assert_eq!(find(&vault, BOTH, "\"valkey").unwrap(), ["IN-1"]);
    assert_eq!(find(&vault, BOTH, "NEAR(valkey)").unwrap(), NONE);
}

#[test]
fn word_endings_and_accents_are_ignored() {
    let vault = vault().unwrap();
    let mut retry = find(&vault, BOTH, "retrying").unwrap();
    retry.sort();
    assert_eq!(retry, ["JD-1", "JD-3"]);
    assert_eq!(find(&vault, BOTH, "vâlkey").unwrap(), ["IN-1"]);
    assert_eq!(find(&vault, BOTH, "PLANS").unwrap(), ["JD-1"]);
}

#[test]
fn title_matches_rank_above_body_matches() {
    let vault = vault().unwrap();
    // JD-3 has "review" in its title; JD-2 only in its body.
    assert_eq!(find(&vault, BOTH, "review").unwrap(), ["JD-3", "JD-2"]);
}

#[test]
fn searches_stay_within_the_given_projects_and_type() {
    let vault = vault().unwrap();
    assert_eq!(find(&vault, &["joedocs"], "valkey").unwrap(), NONE);
    assert_eq!(find(&vault, &[], "valkey").unwrap(), NONE);
    let mut typed = request(BOTH, "redis", &Value::Null);
    typed.doc_type = Some("reviews".into());
    assert_eq!(ids(&vault.store.search(&typed).unwrap()), ["JD-3"]);
    typed.doc_type = Some("all".into());
    assert_eq!(vault.store.search(&typed).unwrap().total, 3);
}

#[test]
fn filters_match_front_matter_keys() {
    let vault = vault().unwrap();
    // Without a query, the most recently updated come first.
    assert_eq!(
        filter(&vault, BOTH, &json!({"tags": {"contains": "security"}})).unwrap(),
        ["JD-3", "JD-1"]
    );
    assert_eq!(
        filter(&vault, BOTH, &json!({"status": "approved"})).unwrap(),
        ["IN-1", "JD-1"]
    );
    assert_eq!(
        filter(&vault, BOTH, &json!({"priority": {"gte": 3}})).unwrap(),
        ["JD-2"]
    );
    assert_eq!(
        filter(&vault, BOTH, &json!({"priority": {"lt": 10, "gt": 1}})).unwrap(),
        ["JD-2", "JD-1"]
    );
    assert_eq!(
        filter(&vault, BOTH, &json!({"approved_by": {"exists": true}})).unwrap(),
        ["JD-3"]
    );
    assert_eq!(
        filter(&vault, BOTH, &json!({"owner": "JOEL"})).unwrap(),
        ["JD-1"]
    );
    assert_eq!(
        filter(&vault, &["joedocs"], &json!({"tags": {"ne": "security"}})).unwrap(),
        ["JD-2"]
    );
    assert_eq!(
        filter(&vault, BOTH, &json!({"tags": {"in": ["cache", "search"]}})).unwrap(),
        ["IN-1", "JD-2"]
    );
    // Server keys are filters too, and dates compare as text.
    assert_eq!(
        filter(
            &vault,
            BOTH,
            &json!({"type": "plans", "created": {"gte": "2026-10-26T07:34"}})
        )
        .unwrap(),
        ["IN-1"]
    );
    assert_eq!(
        filter(&vault, BOTH, &json!({"project": "infra"})).unwrap(),
        ["IN-1"]
    );
    assert_eq!(
        filter(&vault, BOTH, &json!({"revision": 1})).unwrap().len(),
        4
    );
}

#[test]
fn queries_and_filters_combine() {
    let vault = vault().unwrap();
    let results = vault
        .store
        .search(&request(
            BOTH,
            "redis",
            &json!({"tags": {"contains": "security"}, "status": {"ne": "approved"}}),
        ))
        .unwrap();
    assert_eq!(ids(&results), ["JD-3"]);
}

#[test]
fn results_are_paged() {
    let vault = vault().unwrap();
    let mut page = request(BOTH, "", &Value::Null);
    page.limit = 3;
    let first = vault.store.search(&page).unwrap();
    assert_eq!(ids(&first), ["IN-1", "JD-3", "JD-2"]);
    assert_eq!((first.total, first.next_start), (4, Some(4)));
    page.start = 4;
    let second = vault.store.search(&page).unwrap();
    assert_eq!(ids(&second), ["JD-1"]);
    assert_eq!(second.next_start, None);
    assert!(second.items[0].score.is_none() && second.items[0].snippet.is_none());
}

#[test]
fn changes_are_searchable_at_once() {
    let mut vault = vault().unwrap();
    let actor = Actor::new("laptop", "test");
    vault
        .store
        .update(
            &actor,
            Update {
                id: "JD-1".into(),
                content: "Token buckets instead.\n".into(),
                expected_revision: 1,
                front_matter: fields(&json!({"tags": ["throttling"]})),
                summary: None,
            },
        )
        .unwrap();
    assert_eq!(find(&vault, BOTH, "backoff").unwrap(), NONE);
    assert_eq!(find(&vault, BOTH, "buckets").unwrap(), ["JD-1"]);
    assert_eq!(
        filter(&vault, BOTH, &json!({"tags": "throttling"})).unwrap(),
        ["JD-1"]
    );
    assert_eq!(
        filter(&vault, BOTH, &json!({"tags": {"contains": "security"}})).unwrap(),
        ["JD-3"]
    );

    vault
        .store
        .rename(&actor, "JD-1", "Throttling plan", None)
        .unwrap();
    // The old title is an alias, so it still finds the document.
    assert_eq!(find(&vault, BOTH, "limiting").unwrap(), ["JD-1"]);
    assert_eq!(find(&vault, BOTH, "throttling").unwrap(), ["JD-1"]);

    vault
        .store
        .change_front_matter(
            &actor,
            "JD-1",
            &fields(&json!({"status": "done"})),
            &[],
            None,
        )
        .unwrap();
    assert_eq!(
        filter(&vault, BOTH, &json!({"status": "done"})).unwrap(),
        ["JD-1"]
    );
    vault.store.archive(&actor, "JD-1", "done").unwrap();
    assert_eq!(find(&vault, BOTH, "throttling").unwrap(), NONE);
}

#[test]
fn bad_requests_name_the_field() {
    let vault = vault().unwrap();
    let field = |request: &SearchRequest| match vault.store.search(request) {
        Err(Error::Invalid { field, .. }) => field,
        other => panic!("expected an invalid argument, got {other:?}"),
    };
    assert_eq!(field(&request(BOTH, "-redis", &Value::Null)), "query");
    assert_eq!(
        field(&request(BOTH, "", &json!({"tags": {"like": "x"}}))),
        "filters"
    );
    let mut bad = request(BOTH, "", &Value::Null);
    bad.doc_type = Some("plan".into());
    assert_eq!(field(&bad), "type");
    bad.doc_type = None;
    bad.limit = 201;
    assert_eq!(field(&bad), "limit");
    bad.limit = 1;
    bad.start = 0;
    assert_eq!(field(&bad), "start");
    assert!(matches!(
        vault.store.search(&request(&["nope"], "x", &Value::Null)),
        Err(Error::ProjectNotFound(name)) if name == "nope"
    ));
}
