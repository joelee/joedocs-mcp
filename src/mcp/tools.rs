//! The MCP tools.
//!
//! Every tool checks the caller's grant on the project first. A project or
//! document the key cannot see is reported as `not_found`, so its existence
//! does not leak. Tools that change data are recorded in the audit log,
//! including refused attempts.

use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CacheScope, CallToolResult, Implementation, ListToolsResult, PaginatedRequestParams,
    ProtocolVersion, ResultType, ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, Json, RoleServer, ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};

use super::{Caller, Shared, ToolError};
use crate::auth::Permission;
use crate::document::front_matter::Fields;
use crate::store::{
    self, Actor, Changed, DocumentInfo, FileInfo, ListRequest, NewDocument, NewFile, Replacement,
    RevisionInfo, SearchHit, SearchRequest, Sort, Store, Update,
};

/// A tool's result. Tool signatures spell this out, because rmcp's macro
/// reads the return type to generate the output schema.
type ToolResult<T> = Result<Json<T>, CallToolResult>;

/// Instructions sent to clients at initialisation.
const INSTRUCTIONS: &str = "\
JoeDocs stores project documentation (plans, ideas, issues, reviews, research, specs, notes) \
as Markdown with YAML front matter, with full version history.

- Call list_projects first: it shows the projects this key can use, your permission on each, \
and the allowed document types and statuses.
- Documents have stable IDs like JD-42. Titles become file names and can change.
- `content` is the Markdown body only; JoeDocs writes the front matter. Mermaid and PlantUML \
fenced code blocks are kept as written.
- search_documents finds documents by words in the title and body (query) and by front matter \
(filters); use project \"*\" to search every project you can read. Read a hit with get_document.
- update_document needs the revision you last read (expected_revision). On a `conflict` error, \
get_document again, merge, and retry.
- For small changes prefer edit_document (exact old/new replacements) or append_to_document \
over resending the whole body with update_document.
- get_document_history lists revisions; diff_document shows what changed between two, e.g. \
since the revision you last read. restore_document_version brings back an earlier revision as \
a new one.
- Front matter: id, title, project, type, revision, created*, updated*, client are set by the \
server; status, tags and aliases are validated; other keys are yours. Values are strings, \
numbers, booleans or lists of those.
- A multi-file document (new_document with multifile: true) is a folder: its main file plus \
files such as backlog/item-1 that new_document_file adds, each with its own front matter. \
list_document_files lists them; get_document with file reads one. Every change to any file \
raises the document's one revision number.";

/// The MCP server: one instance per session, sharing the store.
#[derive(Clone)]
pub struct JoeDocsServer {
    shared: Arc<Shared>,
    tool_router: ToolRouter<Self>,
}

impl std::fmt::Debug for JoeDocsServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JoeDocsServer").finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------- inputs

/// No arguments.
#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoParams {}

/// Listing order.
#[derive(Debug, Clone, Copy, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SortParam {
    /// Most recently updated first.
    #[default]
    UpdatedDesc,
    /// Least recently updated first.
    UpdatedAsc,
    /// Newest first.
    CreatedDesc,
    /// Oldest first.
    CreatedAsc,
    /// By title.
    Title,
}

impl From<SortParam> for Sort {
    fn from(sort: SortParam) -> Self {
        match sort {
            SortParam::UpdatedDesc => Self::UpdatedDesc,
            SortParam::UpdatedAsc => Self::UpdatedAsc,
            SortParam::CreatedDesc => Self::CreatedDesc,
            SortParam::CreatedAsc => Self::CreatedAsc,
            SortParam::Title => Self::Title,
        }
    }
}

fn all() -> String {
    "all".to_owned()
}
fn default_limit() -> u64 {
    200
}
fn one() -> u64 {
    1
}
fn default_search_limit() -> u64 {
    20
}
fn default_history_limit() -> u64 {
    50
}
fn three() -> u32 {
    3
}

/// Arguments of `list_documents`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListDocumentsParams {
    /// Project name, from `list_projects`.
    pub project: String,
    /// Document type, or "all".
    #[serde(rename = "type", default = "all")]
    pub doc_type: String,
    /// Only documents with this status.
    #[serde(default)]
    pub status: Option<String>,
    /// Most items to return (1 to 1000).
    #[serde(default = "default_limit")]
    pub limit: u64,
    /// 1-based position of the first item; use `next_start` from the previous page.
    #[serde(default = "one")]
    pub start: u64,
    /// Order of the items.
    #[serde(default)]
    pub sort: SortParam,
}

/// Arguments of `search_documents`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchDocumentsParams {
    /// Project name, or "*" for every project this key can read.
    pub project: String,
    /// Document type, or "all".
    #[serde(rename = "type", default = "all")]
    pub doc_type: String,
    /// Words to find in titles, aliases and bodies. All words must match;
    /// "quoted phrase" matches words in order; -word excludes; word* matches
    /// a prefix; a OR b matches either. Word endings are ignored (plans
    /// matches plan). Omit to search by filters only.
    #[serde(default)]
    pub query: Option<String>,
    /// Front matter conditions, all of which must hold, keyed by front
    /// matter key. A plain value means equals, e.g. {"status": "approved"};
    /// otherwise an object of operators: eq, ne, in (a list), contains (for
    /// lists like tags), gt, gte, lt, lte (numbers, or dates as text such as
    /// "2026-10-01"), exists (true or false). Text compares ignoring case.
    /// Example: {"tags": {"contains": "security"}, "updated": {"gte": "2026-10-01"}}.
    #[serde(default)]
    pub filters: Option<serde_json::Map<String, serde_json::Value>>,
    /// Most results to return (1 to 200).
    #[serde(default = "default_search_limit")]
    pub limit: u64,
    /// 1-based position of the first result; use `next_start` from the previous page.
    #[serde(default = "one")]
    pub start: u64,
}

/// Arguments of `get_document_history`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HistoryParams {
    /// Document ID, e.g. JD-42.
    pub document_id: String,
    /// Most revisions to return (1 to 1000), newest first.
    #[serde(default = "default_history_limit")]
    pub limit: u64,
    /// 1-based position of the first revision, newest first; use `next_start` from the previous page.
    #[serde(default = "one")]
    pub start: u64,
}

/// Arguments of `diff_document`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DiffParams {
    /// Document ID, e.g. JD-42.
    pub document_id: String,
    /// The older revision, e.g. the one you last read.
    pub from: u64,
    /// The newer revision; omit for the latest.
    #[serde(default)]
    pub to: Option<u64>,
    /// Unchanged lines to show around each change (0 to 100).
    #[serde(default = "three")]
    pub context: u32,
}

/// Arguments of `restore_document_version`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RestoreParams {
    /// Document ID, e.g. JD-42.
    pub document_id: String,
    /// The revision to bring back.
    pub version: u64,
    /// If given, the restore is refused unless this is the current revision.
    #[serde(default)]
    pub expected_revision: Option<u64>,
    /// One-line description of the change, for the history.
    #[serde(default)]
    pub summary: Option<String>,
}

/// Arguments of `move_document`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MoveParams {
    /// Document ID, e.g. JD-42.
    pub document_id: String,
    /// The new document type (folder), from `list_projects`.
    #[serde(rename = "type")]
    pub doc_type: String,
    /// If given, the move is refused unless this is the current revision.
    #[serde(default)]
    pub expected_revision: Option<u64>,
}

/// One exact replacement.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditParam {
    /// Text to replace. It must occur exactly once in the body, matching
    /// exactly, including whitespace and line breaks.
    pub old: String,
    /// The replacement; empty to delete.
    pub new: String,
}

/// Arguments of `edit_document`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditDocumentParams {
    /// Document ID, e.g. JD-42.
    pub document_id: String,
    /// Replacements in the body (not the front matter), applied in order;
    /// 1 to 64. If any fails, nothing changes.
    pub edits: Vec<EditParam>,
    /// If given, the edit is refused unless this is the current revision.
    #[serde(default)]
    pub expected_revision: Option<u64>,
    /// One-line description of the change, for the history.
    #[serde(default)]
    pub summary: Option<String>,
}

/// Arguments of `append_to_document`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AppendParams {
    /// Document ID, e.g. JD-42.
    pub document_id: String,
    /// Markdown to add at the end of the body, after a blank line.
    pub content: String,
    /// If given, the append is refused unless this is the current revision.
    #[serde(default)]
    pub expected_revision: Option<u64>,
    /// One-line description of the change, for the history.
    #[serde(default)]
    pub summary: Option<String>,
}

/// Arguments of `get_document`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetDocumentParams {
    /// Document ID, e.g. JD-42.
    pub document_id: String,
    /// Revision to read; omit for the latest.
    #[serde(default)]
    pub version: Option<u64>,
    /// For a multi-file document, the file to read, e.g. backlog/item-1;
    /// omit for the main file.
    #[serde(default)]
    pub file: Option<String>,
}

/// Arguments of `new_document_file`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewDocumentFileParams {
    /// Document ID of a multi-file document, e.g. JD-42.
    pub document_id: String,
    /// Path of the new file inside the document's folder, e.g.
    /// backlog/item-1 (with or without .md).
    pub file: String,
    /// Markdown body, without front matter.
    pub content: String,
    /// The file's own front matter: any keys but document, revision and
    /// updated (status here is yours, not checked).
    #[serde(default)]
    #[schemars(with = "Option<serde_json::Map<String, serde_json::Value>>")]
    pub front_matter: Option<Fields>,
    /// The document revision you last read, if you want the call refused
    /// when it is ahead of the document.
    #[serde(default)]
    pub expected_revision: Option<u64>,
    /// One-line description of the change, for the history.
    #[serde(default)]
    pub summary: Option<String>,
}

/// Arguments of `list_document_files`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListDocumentFilesParams {
    /// Document ID, e.g. JD-42.
    pub document_id: String,
    /// Document revision to list the files of; omit for the latest.
    #[serde(default)]
    pub version: Option<u64>,
}

/// Arguments of `new_document`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewDocumentParams {
    /// Project name, from `list_projects`.
    pub project: String,
    /// Document type (a folder); defaults to the configured default type.
    #[serde(rename = "type", default)]
    pub doc_type: Option<String>,
    /// Initial status; defaults to the configured default status.
    #[serde(default)]
    pub status: Option<String>,
    /// Title; also the file name.
    pub title: String,
    /// Markdown body, without front matter.
    pub content: String,
    /// Extra front matter: tags, aliases and your own keys.
    #[serde(default)]
    #[schemars(with = "Option<serde_json::Map<String, serde_json::Value>>")]
    pub front_matter: Option<Fields>,
    /// One-line description of the change, for the history.
    #[serde(default)]
    pub summary: Option<String>,
    /// Create a multi-file document: a folder holding the main file
    /// <title>/<title>.md, to which new_document_file adds files.
    #[serde(default)]
    pub multifile: bool,
}

/// Arguments of `update_document`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateDocumentParams {
    /// Document ID, e.g. JD-42.
    pub document_id: String,
    /// The complete new Markdown body, without front matter.
    pub content: String,
    /// The revision you last read; the update is refused if it is stale.
    pub expected_revision: u64,
    /// Front matter keys to set at the same time (merged, not replaced).
    #[serde(default)]
    #[schemars(with = "Option<serde_json::Map<String, serde_json::Value>>")]
    pub front_matter: Option<Fields>,
    /// One-line description of the change, for the history.
    #[serde(default)]
    pub summary: Option<String>,
}

/// Arguments of `rename_document`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameDocumentParams {
    /// Document ID, e.g. JD-42.
    pub document_id: String,
    /// New title.
    pub title: String,
    /// If given, the rename is refused unless this is the current revision.
    #[serde(default)]
    pub expected_revision: Option<u64>,
}

/// Arguments of `change_document_front_matter`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangeFrontMatterParams {
    /// Document ID, e.g. JD-42.
    pub document_id: String,
    /// Keys to set. Use remove (not null) to delete a key.
    #[serde(default)]
    #[schemars(with = "Option<serde_json::Map<String, serde_json::Value>>")]
    pub set: Option<Fields>,
    /// Custom keys to delete.
    #[serde(default)]
    pub remove: Option<Vec<String>>,
    /// If given, the change is refused unless this is the current revision.
    #[serde(default)]
    pub expected_revision: Option<u64>,
}

/// Arguments of `archive_document`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArchiveDocumentParams {
    /// Document ID, e.g. JD-42.
    pub document_id: String,
    /// Why it is archived, for the history.
    pub reason: String,
}

// --------------------------------------------------------------- outputs

/// A project this key can use.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ProjectOut {
    /// Project name (use it in other tools).
    pub name: String,
    /// Document ID prefix.
    pub key: String,
    /// Title.
    pub title: String,
    /// "read" or "write".
    pub permission: String,
    /// Number of live documents.
    pub documents: u64,
}

/// Result of `list_projects`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ProjectsOut {
    /// Projects this key can use.
    pub projects: Vec<ProjectOut>,
    /// Allowed document types.
    pub types: Vec<String>,
    /// Allowed statuses.
    pub statuses: Vec<String>,
    /// Type used when none is given.
    pub default_type: String,
    /// Status used when none is given.
    pub default_status: String,
}

/// A document's metadata.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct DocumentOut {
    /// Document ID.
    pub id: String,
    /// Project name.
    pub project: String,
    /// Document type.
    #[serde(rename = "type")]
    pub doc_type: String,
    /// Title.
    pub title: String,
    /// Status.
    pub status: String,
    /// Latest revision.
    pub revision: u64,
    /// Path within the vault.
    pub path: String,
    /// Creation time (UTC).
    pub created: String,
    /// Key that created it.
    pub created_by: String,
    /// Time of the latest revision (UTC).
    pub updated: String,
    /// Key that wrote the latest revision.
    pub updated_by: String,
    /// Client that wrote the latest revision.
    pub client: String,
    /// True for a multi-file document (left out otherwise).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub multifile: bool,
    /// For a multi-file document, how many files it has besides the main
    /// file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<u64>,
}

impl From<DocumentInfo> for DocumentOut {
    fn from(doc: DocumentInfo) -> Self {
        Self {
            id: doc.id,
            project: doc.project,
            doc_type: doc.doc_type,
            title: doc.title,
            status: doc.status,
            revision: doc.revision,
            path: doc.path,
            created: doc.created,
            created_by: doc.created_by,
            updated: doc.updated,
            updated_by: doc.updated_by,
            client: doc.client,
            multifile: doc.multifile,
            files: doc.multifile.then_some(doc.files),
        }
    }
}

/// One revision.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct RevisionOut {
    /// Revision number.
    pub revision: u64,
    /// What happened: `create`, `update`, `edit`, `append`, `rename`, `move`,
    /// `front_matter`, `restore`, `archive` or `unarchive`.
    pub action: String,
    /// Key that made it.
    pub author: String,
    /// Client that made it.
    pub client: String,
    /// History summary.
    pub summary: String,
    /// Time (UTC).
    pub at: String,
}

impl From<RevisionInfo> for RevisionOut {
    fn from(rev: RevisionInfo) -> Self {
        Self {
            revision: rev.revision,
            action: rev.action,
            author: rev.author,
            client: rev.client,
            summary: rev.summary,
            at: rev.at,
        }
    }
}

/// Result of `get_document`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct GetDocumentOut {
    /// The whole document: front matter and body.
    pub markdown: String,
    /// Current metadata.
    pub document: DocumentOut,
    /// The revision returned.
    pub revision: RevisionOut,
    /// The file returned, when a file of a multi-file document was asked
    /// for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// For a multi-file document, its files besides the main file at that
    /// revision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
}

/// One file of a document.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct FileOut {
    /// Path inside the document's folder, without .md; the main file's name
    /// for the main file.
    pub file: String,
    /// True for the main file.
    pub main: bool,
    /// The document revision at which the file last changed.
    pub revision: u64,
    /// When it last changed (UTC).
    pub updated: String,
}

impl From<FileInfo> for FileOut {
    fn from(file: FileInfo) -> Self {
        Self {
            file: file.file,
            main: file.main,
            revision: file.revision,
            updated: file.updated,
        }
    }
}

/// Result of `list_document_files`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ListDocumentFilesOut {
    /// Document ID.
    pub document_id: String,
    /// The document revision listed.
    pub revision: u64,
    /// The main file first, then the other files by path.
    pub files: Vec<FileOut>,
}

/// Result of `get_document_history`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct HistoryOut {
    /// Current metadata.
    pub document: DocumentOut,
    /// One page of revisions, newest first.
    pub revisions: Vec<RevisionOut>,
    /// How many revisions there are.
    pub total: u64,
    /// `start` for the next page, if any.
    pub next_start: Option<u64>,
}

/// Result of `diff_document`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct DiffOut {
    /// Document ID.
    pub document_id: String,
    /// The older revision.
    pub from: u64,
    /// The newer revision.
    pub to: u64,
    /// Whether anything differs.
    pub changed: bool,
    /// Unified diff of the whole file, front matter included; empty when
    /// nothing differs.
    pub diff: String,
}

/// Result of `list_documents`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ListDocumentsOut {
    /// One page of documents, without content.
    pub items: Vec<DocumentOut>,
    /// How many documents match.
    pub total: u64,
    /// `start` for the next page, if any.
    pub next_start: Option<u64>,
}

/// One search result.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SearchHitOut {
    /// The document's metadata.
    #[serde(flatten)]
    pub document: DocumentOut,
    /// Relevance, higher is better; absent without a query.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    /// The best-matching part of the body, with matches marked «like this»;
    /// absent without a query.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

impl From<SearchHit> for SearchHitOut {
    fn from(hit: SearchHit) -> Self {
        Self {
            document: hit.document.into(),
            score: hit.score,
            snippet: hit.snippet,
        }
    }
}

/// Result of `search_documents`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SearchDocumentsOut {
    /// Best matches first (most recently updated first without a query).
    pub items: Vec<SearchHitOut>,
    /// How many documents match.
    pub total: u64,
    /// `start` for the next page, if any.
    pub next_start: Option<u64>,
}

/// Result of a document change.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ChangedOut {
    /// Document ID.
    pub document_id: String,
    /// The new revision.
    pub revision: u64,
    /// Path within the vault.
    pub path: String,
    /// The file changed, for a file of a multi-file document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// The complete front matter now in the document (or file).
    #[schemars(with = "serde_json::Map<String, serde_json::Value>")]
    pub front_matter: Fields,
}

impl From<Changed> for ChangedOut {
    fn from(changed: Changed) -> Self {
        Self {
            document_id: changed.id,
            revision: changed.revision,
            path: changed.path,
            file: changed.file,
            front_matter: changed.front_matter,
        }
    }
}

/// Result of `archive_document`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ArchivedOut {
    /// Document ID.
    pub document_id: String,
    /// The archive revision.
    pub revision: u64,
    /// When it was archived (UTC).
    pub archived_at: String,
}

// ----------------------------------------------------------------- tools

fn identify(ctx: &RequestContext<RoleServer>) -> Result<(Caller, Actor), ToolError> {
    let caller = ctx
        .extensions
        .get::<axum::http::request::Parts>()
        .and_then(|parts| parts.extensions.get::<Caller>())
        .cloned()
        .ok_or_else(ToolError::unauthenticated)?;
    let client = ctx
        .meta
        .client_info()
        .map(|info| info.name)
        .or_else(|| {
            ctx.peer
                .peer_info()
                .map(|info| info.client_info.name.clone())
        })
        .unwrap_or_default();
    let actor = Actor::new(caller.name.clone(), &client);
    Ok((caller, actor))
}

fn require_project(caller: &Caller, project: &str, needed: Permission) -> Result<(), ToolError> {
    match caller.permission(project) {
        Some(permission) if permission.allows(needed) => Ok(()),
        Some(_) => Err(ToolError::permission_denied(project)),
        None => Err(store::Error::ProjectNotFound(project.to_owned()).into()),
    }
}

fn require_document(
    store: &Store,
    caller: &Caller,
    id: &str,
    needed: Permission,
) -> Result<(), ToolError> {
    let not_found = || ToolError::from(store::Error::NotFound(id.trim().to_owned()));
    let project = store.project_of_document(id)?.ok_or_else(not_found)?;
    match caller.permission(&project.name) {
        Some(permission) if permission.allows(needed) => Ok(()),
        Some(_) => Err(ToolError::permission_denied(&project.name)),
        None => Err(not_found()),
    }
}

impl JoeDocsServer {
    /// A server over the shared store.
    #[must_use]
    pub fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            tool_router: Self::tool_router(),
        }
    }

    /// Runs a read-only operation.
    async fn read<T, F>(&self, ctx: &RequestContext<RoleServer>, f: F) -> ToolResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Store, &Caller) -> Result<T, ToolError> + Send + 'static,
    {
        let result = async {
            let (caller, _) = identify(ctx)?;
            self.shared.with_store(move |store| f(store, &caller)).await
        }
        .await;
        result.map(Json).map_err(ToolError::into_result)
    }

    /// Runs a change and records it, or the refusal, in the audit log.
    async fn write<T, F>(
        &self,
        ctx: &RequestContext<RoleServer>,
        tool: &'static str,
        document_id: Option<String>,
        f: F,
    ) -> ToolResult<T>
    where
        T: AuditId + Send + 'static,
        F: FnOnce(&mut Store, &Caller, &Actor) -> Result<T, ToolError> + Send + 'static,
    {
        let result = async {
            let (caller, actor) = identify(ctx)?;
            self.shared
                .with_store(move |store| {
                    let result = f(store, &caller, &actor);
                    let (outcome, id) = match &result {
                        Ok(value) => ("ok", value.audit_id().or(document_id)),
                        Err(error) => (error.code, document_id),
                    };
                    if let Err(error) =
                        store.audit(&caller.key_id, &actor, tool, id.as_deref(), outcome)
                    {
                        tracing::warn!(%error, tool, "cannot write the audit log");
                    }
                    result
                })
                .await
        }
        .await;
        result.map(Json).map_err(ToolError::into_result)
    }
}

/// The document a change result refers to, for the audit log.
trait AuditId {
    fn audit_id(&self) -> Option<String>;
}

impl AuditId for ChangedOut {
    fn audit_id(&self) -> Option<String> {
        Some(self.document_id.clone())
    }
}

impl AuditId for ArchivedOut {
    fn audit_id(&self) -> Option<String> {
        Some(self.document_id.clone())
    }
}

#[tool_router(router = tool_router)]
impl JoeDocsServer {
    #[tool(
        description = "List the projects this API key can use, with your permission on each and the allowed document types and statuses. Call this first.",
        annotations(read_only_hint = true)
    )]
    async fn list_projects(
        &self,
        Parameters(_): Parameters<NoParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<ProjectsOut>, CallToolResult> {
        let settings = self.shared.settings.clone();
        self.read(&ctx, move |store, caller| {
            let projects = store
                .projects()?
                .into_iter()
                .filter_map(|p| {
                    caller.permission(&p.name).map(|permission| ProjectOut {
                        permission: permission.to_string(),
                        name: p.name,
                        key: p.key,
                        title: p.title,
                        documents: p.documents,
                    })
                })
                .collect();
            Ok(ProjectsOut {
                projects,
                types: settings.types,
                statuses: settings.statuses,
                default_type: settings.default_type,
                default_status: settings.default_status,
            })
        })
        .await
    }

    #[tool(
        description = "List a project's documents (metadata only, no content), optionally filtered by type and status, one page at a time.",
        annotations(read_only_hint = true)
    )]
    async fn list_documents(
        &self,
        Parameters(params): Parameters<ListDocumentsParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<ListDocumentsOut>, CallToolResult> {
        self.read(&ctx, move |store, caller| {
            require_project(caller, &params.project, Permission::Read)?;
            let listing = store.list(&ListRequest {
                project: params.project,
                doc_type: Some(params.doc_type),
                status: params.status,
                sort: params.sort.into(),
                start: params.start,
                limit: params.limit,
            })?;
            Ok(ListDocumentsOut {
                items: listing.items.into_iter().map(DocumentOut::from).collect(),
                total: listing.total,
                next_start: listing.next_start,
            })
        })
        .await
    }

    #[tool(
        description = "Search documents by words in their title and body (query) and by front matter (filters), in one project or in every project you can read (project \"*\"). Returns metadata, a relevance score and a snippet per hit, best first; read a hit with get_document.",
        annotations(read_only_hint = true)
    )]
    async fn search_documents(
        &self,
        Parameters(params): Parameters<SearchDocumentsParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<SearchDocumentsOut>, CallToolResult> {
        self.read(&ctx, move |store, caller| {
            let projects = if params.project == "*" {
                caller.grants.iter().map(|g| g.project.clone()).collect()
            } else {
                require_project(caller, &params.project, Permission::Read)?;
                vec![params.project]
            };
            let results = store.search(&SearchRequest {
                projects,
                doc_type: Some(params.doc_type),
                query: params.query.unwrap_or_default(),
                filters: params.filters.unwrap_or_default(),
                start: params.start,
                limit: params.limit,
            })?;
            Ok(SearchDocumentsOut {
                items: results.items.into_iter().map(SearchHitOut::from).collect(),
                total: results.total,
                next_start: results.next_start,
            })
        })
        .await
    }

    #[tool(
        description = "Read a document: the whole Markdown file (front matter and body) and its metadata. Pass version to read an earlier revision.",
        annotations(read_only_hint = true)
    )]
    async fn get_document(
        &self,
        Parameters(params): Parameters<GetDocumentParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<GetDocumentOut>, CallToolResult> {
        self.read(&ctx, move |store, caller| {
            require_document(store, caller, &params.document_id, Permission::Read)?;
            let fetched = match &params.file {
                Some(file) => store.get_file(&params.document_id, file, params.version)?,
                None => store.get(&params.document_id, params.version)?,
            };
            let multifile = fetched.document.multifile;
            Ok(GetDocumentOut {
                markdown: fetched.markdown,
                document: fetched.document.into(),
                revision: fetched.revision.into(),
                file: fetched.file,
                files: multifile.then_some(fetched.files),
            })
        })
        .await
    }

    #[tool(
        description = "List a document's revisions, newest first: revision number, action, author key, client, summary and time.",
        annotations(read_only_hint = true)
    )]
    async fn get_document_history(
        &self,
        Parameters(params): Parameters<HistoryParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<HistoryOut>, CallToolResult> {
        self.read(&ctx, move |store, caller| {
            require_document(store, caller, &params.document_id, Permission::Read)?;
            let page = store.history(&params.document_id, params.start, params.limit)?;
            Ok(HistoryOut {
                document: page.document.into(),
                revisions: page.revisions.into_iter().map(RevisionOut::from).collect(),
                total: page.total,
                next_start: page.next_start,
            })
        })
        .await
    }

    #[tool(
        description = "Show what changed in a document between two revisions as a unified diff of the whole file (front matter included). Omit `to` to compare with the latest, e.g. to see what changed since you last read it.",
        annotations(read_only_hint = true)
    )]
    async fn diff_document(
        &self,
        Parameters(params): Parameters<DiffParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<DiffOut>, CallToolResult> {
        self.read(&ctx, move |store, caller| {
            require_document(store, caller, &params.document_id, Permission::Read)?;
            let diff = store.diff(&params.document_id, params.from, params.to, params.context)?;
            Ok(DiffOut {
                document_id: diff.id,
                from: diff.from,
                to: diff.to,
                changed: !diff.diff.is_empty(),
                diff: diff.diff,
            })
        })
        .await
    }

    #[tool(
        description = "Create a document at revision 1. Give the Markdown body in content; the server writes the front matter. Returns the new document ID.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    async fn new_document(
        &self,
        Parameters(params): Parameters<NewDocumentParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<ChangedOut>, CallToolResult> {
        self.write(&ctx, "new_document", None, move |store, caller, actor| {
            require_project(caller, &params.project, Permission::Write)?;
            let changed = store.create(
                actor,
                NewDocument {
                    project: params.project,
                    doc_type: params.doc_type,
                    status: params.status,
                    title: params.title,
                    content: params.content,
                    front_matter: params.front_matter.unwrap_or_default(),
                    summary: params.summary,
                    multifile: params.multifile,
                },
            )?;
            Ok(changed.into())
        })
        .await
    }

    #[tool(
        description = "Add a file to a multi-file document (one made with new_document multifile: true), e.g. backlog/item-1, with its own front matter. Raises the document's revision by one; give expected_revision (the document revision you last read) to be refused if the document moved past it. Returns the new revision.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    async fn new_document_file(
        &self,
        Parameters(params): Parameters<NewDocumentFileParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<ChangedOut>, CallToolResult> {
        let id = params.document_id.clone();
        self.write(
            &ctx,
            "new_document_file",
            Some(id),
            move |store, caller, actor| {
                require_document(store, caller, &params.document_id, Permission::Write)?;
                let changed = store.create_file(
                    actor,
                    &NewFile {
                        document_id: params.document_id,
                        file: params.file,
                        content: params.content,
                        front_matter: params.front_matter.unwrap_or_default(),
                        expected_revision: params.expected_revision,
                        summary: params.summary,
                    },
                )?;
                Ok(changed.into())
            },
        )
        .await
    }

    #[tool(
        description = "List a document's files: the main file first, then the other files of a multi-file document by path, each with the document revision at which it last changed. Pass version to list them as they were at an earlier revision.",
        annotations(read_only_hint = true)
    )]
    async fn list_document_files(
        &self,
        Parameters(params): Parameters<ListDocumentFilesParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<ListDocumentFilesOut>, CallToolResult> {
        self.read(&ctx, move |store, caller| {
            require_document(store, caller, &params.document_id, Permission::Read)?;
            let files = store.list_files(&params.document_id, params.version)?;
            let revision = match params.version {
                Some(version) => version,
                None => store.get(&params.document_id, None)?.document.revision,
            };
            Ok(ListDocumentFilesOut {
                document_id: params.document_id.trim().to_owned(),
                revision,
                files: files.into_iter().map(FileOut::from).collect(),
            })
        })
        .await
    }

    #[tool(
        description = "Replace a document's body. expected_revision must be the revision you last read; on a conflict error, read it again and merge. front_matter keys given here are set as in change_document_front_matter. A call that changes neither the body nor the front matter is refused with invalid_argument (field content), and no revision is written.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false
        )
    )]
    async fn update_document(
        &self,
        Parameters(params): Parameters<UpdateDocumentParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<ChangedOut>, CallToolResult> {
        let id = params.document_id.clone();
        self.write(
            &ctx,
            "update_document",
            Some(id),
            move |store, caller, actor| {
                require_document(store, caller, &params.document_id, Permission::Write)?;
                let changed = store.update(
                    actor,
                    Update {
                        id: params.document_id,
                        content: params.content,
                        expected_revision: params.expected_revision,
                        front_matter: params.front_matter.unwrap_or_default(),
                        summary: params.summary,
                    },
                )?;
                Ok(changed.into())
            },
        )
        .await
    }

    #[tool(
        description = "Change a document's title. The file is renamed and the old title is kept in aliases.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    async fn rename_document(
        &self,
        Parameters(params): Parameters<RenameDocumentParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<ChangedOut>, CallToolResult> {
        let id = params.document_id.clone();
        self.write(
            &ctx,
            "rename_document",
            Some(id),
            move |store, caller, actor| {
                require_document(store, caller, &params.document_id, Permission::Write)?;
                let changed = store.rename(
                    actor,
                    &params.document_id,
                    &params.title,
                    params.expected_revision,
                )?;
                Ok(changed.into())
            },
        )
        .await
    }

    #[tool(
        description = "Set and remove front matter keys without touching the body. status, tags and aliases can be set but not removed; server keys (id, title, type, revision, ...) cannot be changed here. A call that would change nothing is refused with invalid_argument (field front_matter).",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false
        )
    )]
    async fn change_document_front_matter(
        &self,
        Parameters(params): Parameters<ChangeFrontMatterParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<ChangedOut>, CallToolResult> {
        let id = params.document_id.clone();
        self.write(
            &ctx,
            "change_document_front_matter",
            Some(id),
            move |store, caller, actor| {
                require_document(store, caller, &params.document_id, Permission::Write)?;
                let changed = store.change_front_matter(
                    actor,
                    &params.document_id,
                    &params.set.unwrap_or_default(),
                    &params.remove.unwrap_or_default(),
                    params.expected_revision,
                )?;
                Ok(changed.into())
            },
        )
        .await
    }

    #[tool(
        description = "Change a document's body with exact replacements, without resending it: each `old` must occur exactly once in the body (not the front matter). Edits apply in order; if any fails, nothing changes and the error names it. Edits that leave the body as it was are refused with invalid_argument (field edits).",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false
        )
    )]
    async fn edit_document(
        &self,
        Parameters(params): Parameters<EditDocumentParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<ChangedOut>, CallToolResult> {
        let id = params.document_id.clone();
        self.write(
            &ctx,
            "edit_document",
            Some(id),
            move |store, caller, actor| {
                require_document(store, caller, &params.document_id, Permission::Write)?;
                let edits: Vec<Replacement> = params
                    .edits
                    .into_iter()
                    .map(|e| Replacement {
                        old: e.old,
                        new: e.new,
                    })
                    .collect();
                let changed = store.edit(
                    actor,
                    &params.document_id,
                    &edits,
                    params.expected_revision,
                    params.summary.as_deref(),
                )?;
                Ok(changed.into())
            },
        )
        .await
    }

    #[tool(
        description = "Add Markdown to the end of a document's body, after a blank line, e.g. a comment on an issue or a log entry.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    async fn append_to_document(
        &self,
        Parameters(params): Parameters<AppendParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<ChangedOut>, CallToolResult> {
        let id = params.document_id.clone();
        self.write(
            &ctx,
            "append_to_document",
            Some(id),
            move |store, caller, actor| {
                require_document(store, caller, &params.document_id, Permission::Write)?;
                let changed = store.append(
                    actor,
                    &params.document_id,
                    &params.content,
                    params.expected_revision,
                    params.summary.as_deref(),
                )?;
                Ok(changed.into())
            },
        )
        .await
    }

    #[tool(
        description = "Change a document's type (its folder), e.g. from ideas to plans. The ID, title and content stay the same.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    async fn move_document(
        &self,
        Parameters(params): Parameters<MoveParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<ChangedOut>, CallToolResult> {
        let id = params.document_id.clone();
        self.write(
            &ctx,
            "move_document",
            Some(id),
            move |store, caller, actor| {
                require_document(store, caller, &params.document_id, Permission::Write)?;
                let changed = store.move_document(
                    actor,
                    &params.document_id,
                    &params.doc_type,
                    params.expected_revision,
                )?;
                Ok(changed.into())
            },
        )
        .await
    }

    #[tool(
        description = "Bring back an earlier revision's body, status, tags and custom front matter as a new revision. The title, type and aliases stay as they are, and the history keeps every revision.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false
        )
    )]
    async fn restore_document_version(
        &self,
        Parameters(params): Parameters<RestoreParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<ChangedOut>, CallToolResult> {
        let id = params.document_id.clone();
        self.write(
            &ctx,
            "restore_document_version",
            Some(id),
            move |store, caller, actor| {
                require_document(store, caller, &params.document_id, Permission::Write)?;
                let changed = store.restore_version(
                    actor,
                    &params.document_id,
                    params.version,
                    params.expected_revision,
                    params.summary.as_deref(),
                )?;
                Ok(changed.into())
            },
        )
        .await
    }

    #[tool(
        description = "Archive a document: it disappears from listings and the vault, but its history is kept and an administrator can restore it.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false
        )
    )]
    async fn archive_document(
        &self,
        Parameters(params): Parameters<ArchiveDocumentParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<ArchivedOut>, CallToolResult> {
        let id = params.document_id.clone();
        self.write(
            &ctx,
            "archive_document",
            Some(id),
            move |store, caller, actor| {
                require_document(store, caller, &params.document_id, Permission::Write)?;
                let archived = store.archive(actor, &params.document_id, &params.reason)?;
                Ok(ArchivedOut {
                    document_id: archived.id,
                    revision: archived.revision,
                    archived_at: archived.archived_at,
                })
            },
        )
        .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for JoeDocsServer {
    // The macro would generate this as an `async fn` with no `.await`.
    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + Send + '_ {
        let cache_hints = context
            .protocol_version()
            .is_some_and(|version| version >= ProtocolVersion::V_2026_07_28);
        std::future::ready(Ok(ListToolsResult {
            result_type: Some(ResultType::COMPLETE),
            tools: self.tool_router.list_all(),
            meta: None,
            next_cursor: None,
            ttl_ms: cache_hints.then_some(0),
            cache_scope: cache_hints.then_some(CacheScope::Public),
        }))
    }

    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("joedocs", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}
