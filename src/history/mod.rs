//! Version history: the [`History`] trait (one commit per revision) and
//! [`GitCli`], its implementation over the `git` command line, using a git
//! directory kept outside the vault.
//!
//! See `docs/plan.md` section 6.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use jiff::Timestamp;

/// A history operation failed.
#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    /// `git` could not be started (for example, it is not installed).
    #[error("cannot run git: {0}")]
    Spawn(#[source] io::Error),
    /// A `git` command exited with an error.
    #[error("git {command} failed: {stderr}")]
    Failed {
        /// The git subcommand, e.g. `commit`.
        command: String,
        /// The first line of git's error output.
        stderr: String,
    },
    /// A requested revision or path does not exist in the history.
    #[error("{0} is not in the history")]
    Missing(String),
}

/// One commit: the changed vault paths and who made the change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    /// Paths relative to the vault, with `/` separators. Deleted files are
    /// listed too.
    pub paths: Vec<String>,
    /// Author name (an API key name).
    pub author: String,
    /// Commit time.
    pub at: Timestamp,
    /// Full commit message, including trailers.
    pub message: String,
}

/// How a commit changed one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// The file was added.
    Added,
    /// The file was modified.
    Modified,
    /// The file was deleted.
    Deleted,
}

/// A commit read back from the history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Logged {
    /// Commit ID.
    pub id: String,
    /// Author name.
    pub author: String,
    /// Author time.
    pub at: Timestamp,
    /// First line of the message.
    pub subject: String,
    /// Message trailers (`Key: value`), in order.
    pub trailers: Vec<(String, String)>,
    /// Changed paths, relative to the vault. Renames are a deletion and an
    /// addition.
    pub changes: Vec<(Change, String)>,
}

impl Logged {
    /// The value of the first trailer named `key`.
    #[must_use]
    pub fn trailer(&self, key: &str) -> Option<&str> {
        self.trailers
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Version history of the vault.
pub trait History: Send {
    /// Whether the history store exists.
    fn exists(&self) -> bool;

    /// Creates an empty history store. Does nothing if it exists.
    ///
    /// # Errors
    ///
    /// Returns an error when the store cannot be created.
    fn init(&self) -> Result<(), HistoryError>;

    /// The latest commit, or `None` before the first commit.
    ///
    /// # Errors
    ///
    /// Returns an error when the history cannot be read.
    fn head(&self) -> Result<Option<String>, HistoryError>;

    /// Records the current state of `commit.paths` as one commit and returns
    /// its ID. On failure nothing is recorded or left staged.
    ///
    /// # Errors
    ///
    /// Returns an error when staging or committing fails.
    fn commit(&self, commit: &Commit) -> Result<String, HistoryError>;

    /// The contents of `path` as of commit `id`.
    ///
    /// # Errors
    ///
    /// Returns [`HistoryError::Missing`] when the commit or path does not
    /// exist.
    fn show(&self, id: &str, path: &str) -> Result<Vec<u8>, HistoryError>;

    /// Every commit, oldest first.
    ///
    /// # Errors
    ///
    /// Returns an error when the history cannot be read.
    fn log(&self) -> Result<Vec<Logged>, HistoryError>;
}

/// [`History`] over the `git` command line, with the git directory outside
/// the work tree (the vault).
///
/// Every command runs with the user's and system's git configuration
/// ignored, so hooks, signing and aliases cannot interfere, and with
/// literal pathspecs, so file names are never treated as patterns.
#[derive(Debug, Clone)]
pub struct GitCli {
    program: PathBuf,
    git_dir: PathBuf,
    work_tree: PathBuf,
}

/// Domain used for commit e-mail addresses (reserved, never routable).
const EMAIL_DOMAIN: &str = "joedocs.invalid";

impl GitCli {
    /// Uses `git` from `PATH` with the given git directory and work tree.
    #[must_use]
    pub fn new(git_dir: impl Into<PathBuf>, work_tree: impl Into<PathBuf>) -> Self {
        Self::with_program("git", git_dir, work_tree)
    }

    /// Uses a specific `git` executable.
    #[must_use]
    pub fn with_program(
        program: impl Into<PathBuf>,
        git_dir: impl Into<PathBuf>,
        work_tree: impl Into<PathBuf>,
    ) -> Self {
        Self {
            program: program.into(),
            git_dir: git_dir.into(),
            work_tree: work_tree.into(),
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.arg("--git-dir")
            .arg(&self.git_dir)
            .arg("--work-tree")
            .arg(&self.work_tree)
            .args(["-c", "core.quotepath=false", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(&self.work_tree)
            .env("GIT_CONFIG_GLOBAL", null_device())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_LITERAL_PATHSPECS", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C");
        for var in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_NAMESPACE",
        ] {
            cmd.env_remove(var);
        }
        cmd
    }

    fn run(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<Output, HistoryError> {
        let mut cmd = self.command(args);
        cmd.stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(HistoryError::Spawn)?;
        if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
            pipe.write_all(input).map_err(HistoryError::Spawn)?;
        }
        child.wait_with_output().map_err(HistoryError::Spawn)
    }

    fn checked(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<Output, HistoryError> {
        let output = self.run(args, stdin)?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(failure(args, &output))
        }
    }

    fn stage_and_commit(&self, commit: &Commit) -> Result<(), HistoryError> {
        let mut add = vec!["add", "--all", "--"];
        add.extend(commit.paths.iter().map(String::as_str));
        self.checked(&add, None)?;

        let email = format!("{}@{EMAIL_DOMAIN}", commit.author);
        let date = format!("@{} +0000", commit.at.as_second());
        let mut cmd = self.command(&["commit", "--quiet", "--no-verify", "--file", "-"]);
        cmd.env("GIT_AUTHOR_NAME", &commit.author)
            .env("GIT_AUTHOR_EMAIL", &email)
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_NAME", &commit.author)
            .env("GIT_COMMITTER_EMAIL", &email)
            .env("GIT_COMMITTER_DATE", &date)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(HistoryError::Spawn)?;
        if let Some(mut pipe) = child.stdin.take() {
            pipe.write_all(commit.message.as_bytes())
                .map_err(HistoryError::Spawn)?;
        }
        let output = child.wait_with_output().map_err(HistoryError::Spawn)?;
        if output.status.success() {
            Ok(())
        } else {
            Err(failure(&["commit"], &output))
        }
    }
}

fn null_device() -> &'static Path {
    Path::new("/dev/null")
}

fn failure(args: &[&str], output: &Output) -> HistoryError {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let message = stderr
        .lines()
        .chain(stdout.lines())
        .find(|line| !line.trim().is_empty())
        .unwrap_or("no output")
        .trim()
        .to_owned();
    HistoryError::Failed {
        command: args.first().copied().unwrap_or_default().to_owned(),
        stderr: message,
    }
}

fn stdout_line(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

impl History for GitCli {
    fn exists(&self) -> bool {
        self.git_dir.join("HEAD").is_file()
    }

    fn init(&self) -> Result<(), HistoryError> {
        if self.exists() {
            return Ok(());
        }
        let output = Command::new(&self.program)
            .arg("--git-dir")
            .arg(&self.git_dir)
            .args(["init", "--quiet", "--initial-branch", "main"])
            .env("GIT_CONFIG_GLOBAL", null_device())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .stdin(Stdio::null())
            .output()
            .map_err(HistoryError::Spawn)?;
        if output.status.success() {
            Ok(())
        } else {
            Err(failure(&["init"], &output))
        }
    }

    fn head(&self) -> Result<Option<String>, HistoryError> {
        let output = self.run(&["rev-parse", "--quiet", "--verify", "HEAD^{commit}"], None)?;
        if output.status.success() {
            Ok(Some(stdout_line(&output)))
        } else if output.status.code() == Some(1) && self.exists() {
            Ok(None)
        } else {
            Err(failure(&["rev-parse"], &output))
        }
    }

    fn commit(&self, commit: &Commit) -> Result<String, HistoryError> {
        if let Err(error) = self.stage_and_commit(commit) {
            let mut reset = vec!["reset", "--quiet", "--"];
            reset.extend(commit.paths.iter().map(String::as_str));
            if let Err(reset_error) = self.checked(&reset, None) {
                tracing::warn!(%reset_error, "cannot unstage after a failed commit");
            }
            return Err(error);
        }
        let output = self.checked(&["rev-parse", "--verify", "HEAD^{commit}"], None)?;
        Ok(stdout_line(&output))
    }

    fn show(&self, id: &str, path: &str) -> Result<Vec<u8>, HistoryError> {
        let object = format!("{id}:{path}");
        let exists = self.run(&["cat-file", "-e", &object], None)?;
        if !exists.status.success() {
            return Err(HistoryError::Missing(object));
        }
        Ok(self.checked(&["cat-file", "blob", &object], None)?.stdout)
    }

    fn log(&self) -> Result<Vec<Logged>, HistoryError> {
        if self.head()?.is_none() {
            return Ok(Vec::new());
        }
        let output = self.checked(
            &[
                "log",
                "--reverse",
                "--no-renames",
                "--name-status",
                "-z",
                LOG_FORMAT,
            ],
            None,
        )?;
        parse_log(&String::from_utf8_lossy(&output.stdout))
    }
}

/// `git log` format: a record separator, then NUL-separated fields. With
/// `-z`, the changed paths follow as NUL-separated status and path pairs.
const LOG_FORMAT: &str =
    "--format=%x1e%H%x00%an%x00%at%x00%s%x00%(trailers:only,unfold,separator=%x1f)%x00";

fn parse_log(text: &str) -> Result<Vec<Logged>, HistoryError> {
    let malformed = |what: &str| HistoryError::Failed {
        command: "log".to_owned(),
        stderr: format!("unexpected output: {what}"),
    };
    let mut commits = Vec::new();
    for record in text.split('\x1e').skip(1) {
        let mut fields = record.split('\0');
        let mut next = || fields.next().ok_or_else(|| malformed("a short record"));
        let id = next()?.to_owned();
        let author = next()?.to_owned();
        let at = next()?
            .parse::<i64>()
            .ok()
            .and_then(|secs| Timestamp::from_second(secs).ok())
            .ok_or_else(|| malformed("a bad author time"))?;
        let subject = next()?.to_owned();
        let trailers = next()?
            .split('\x1f')
            .filter_map(|line| line.split_once(':'))
            .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
            .collect();
        let mut changes = Vec::new();
        let mut rest = fields.filter(|field| !field.trim().is_empty());
        while let Some(status) = rest.next() {
            let change = match status.trim() {
                "A" => Change::Added,
                "M" | "T" => Change::Modified,
                "D" => Change::Deleted,
                other => return Err(malformed(&format!("change status {other}"))),
            };
            let path = rest
                .next()
                .ok_or_else(|| malformed("a status without a path"))?;
            changes.push((change, path.to_owned()));
        }
        commits.push(Logged {
            id,
            author,
            at,
            subject,
            trailers,
            changes,
        });
    }
    Ok(commits)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    struct Repo {
        _dir: tempfile::TempDir,
        home: PathBuf,
        git: GitCli,
    }

    fn repo() -> Repo {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        fs::create_dir(&home).unwrap();
        fs::create_dir(dir.path().join("state")).unwrap();
        let git = GitCli::new(dir.path().join("state/history.git"), &home);
        git.init().unwrap();
        Repo {
            _dir: dir,
            home,
            git,
        }
    }

    fn commit(paths: &[&str], message: &str) -> Commit {
        Commit {
            paths: paths.iter().map(|p| (*p).to_owned()).collect(),
            author: "laptop".into(),
            at: Timestamp::from_second(1_793_000_000).unwrap(),
            message: message.into(),
        }
    }

    fn git_out(repo: &Repo, args: &[&str]) -> String {
        let output = repo.git.checked(args, None).unwrap();
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn init_is_idempotent_and_keeps_the_vault_clean() {
        let repo = repo();
        assert!(repo.git.exists());
        repo.git.init().unwrap();
        assert_eq!(fs::read_dir(&repo.home).unwrap().count(), 0);
        assert_eq!(repo.git.head().unwrap(), None);
    }

    #[test]
    fn commits_record_author_time_message_and_content() {
        let repo = repo();
        fs::create_dir_all(repo.home.join("p/plans")).unwrap();
        fs::write(repo.home.join("p/plans/A [b].md"), "one").unwrap();
        let message = "Create JD-1: A\n\nJoeDocs-Document: JD-1\nJoeDocs-Revision: 1\n";
        let id = repo
            .git
            .commit(&commit(&["p/plans/A [b].md"], message))
            .unwrap();
        assert_eq!(repo.git.head().unwrap().as_deref(), Some(id.as_str()));
        assert_eq!(repo.git.show(&id, "p/plans/A [b].md").unwrap(), b"one");

        let log = git_out(
            &repo,
            &[
                "log",
                "--format=%an <%ae> %aI|%cn|%(trailers:key=JoeDocs-Revision,valueonly)",
            ],
        );
        assert_eq!(
            log.trim(),
            "laptop <laptop@joedocs.invalid> 2026-10-26T07:33:20Z|laptop|1"
        );
    }

    #[test]
    fn deletions_and_renames_are_committed() {
        let repo = repo();
        fs::write(repo.home.join("a.md"), "x").unwrap();
        let first = repo.git.commit(&commit(&["a.md"], "add")).unwrap();
        fs::rename(repo.home.join("a.md"), repo.home.join("b.md")).unwrap();
        let second = repo
            .git
            .commit(&commit(&["a.md", "b.md"], "rename"))
            .unwrap();
        assert_ne!(first, second);
        assert!(matches!(
            repo.git.show(&second, "a.md"),
            Err(HistoryError::Missing(_))
        ));
        assert_eq!(repo.git.show(&second, "b.md").unwrap(), b"x");
        assert_eq!(repo.git.show(&first, "a.md").unwrap(), b"x");
        assert_eq!(git_out(&repo, &["status", "--porcelain"]), "");
    }

    #[test]
    fn a_failed_commit_leaves_nothing_staged() {
        let repo = repo();
        fs::write(repo.home.join("a.md"), "x").unwrap();
        let err = repo.git.commit(&commit(&["a.md"], "")).unwrap_err();
        assert!(
            matches!(&err, HistoryError::Failed { command, .. } if command == "commit"),
            "{err}"
        );
        assert_eq!(git_out(&repo, &["status", "--porcelain"]), "?? a.md\n");
        assert_eq!(repo.git.head().unwrap(), None);
    }

    #[test]
    fn staging_a_missing_path_fails() {
        let repo = repo();
        let err = repo.git.commit(&commit(&["nope.md"], "x")).unwrap_err();
        assert!(err.to_string().starts_with("git add failed: "), "{err}");
    }

    #[test]
    fn missing_git_or_repository_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let git = GitCli::with_program("/nonexistent/git", dir.path().join("h.git"), dir.path());
        assert!(matches!(git.init(), Err(HistoryError::Spawn(_))));
        assert!(
            git.head()
                .unwrap_err()
                .to_string()
                .starts_with("cannot run git")
        );

        let git = GitCli::new(dir.path().join("missing.git"), dir.path());
        assert!(!git.exists());
        assert!(matches!(git.head(), Err(HistoryError::Failed { .. })));

        let blocked = dir.path().join("file");
        fs::write(&blocked, "").unwrap();
        let git = GitCli::new(blocked.join("h.git"), dir.path());
        assert!(
            git.init()
                .unwrap_err()
                .to_string()
                .starts_with("git init failed")
        );
    }

    #[test]
    fn the_log_lists_commits_oldest_first_with_trailers_and_changes() {
        let repo = repo();
        assert_eq!(repo.git.log().unwrap(), []);
        fs::create_dir_all(repo.home.join("p/plans")).unwrap();
        fs::write(repo.home.join("p/plans/A b.md"), "one").unwrap();
        let message = "Create JD-1: A b\n\nJoeDocs-Document: JD-1\nJoeDocs-Client: claude code\n";
        let first = repo
            .git
            .commit(&commit(&["p/plans/A b.md"], message))
            .unwrap();
        fs::rename(
            repo.home.join("p/plans/A b.md"),
            repo.home.join("p/plans/B.md"),
        )
        .unwrap();
        fs::write(repo.home.join("other.md"), "x").unwrap();
        let second = repo
            .git
            .commit(&commit(
                &["p/plans/A b.md", "p/plans/B.md", "other.md"],
                "Rename",
            ))
            .unwrap();

        let log = repo.git.log().unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].id, first);
        assert_eq!(log[0].author, "laptop");
        assert_eq!(log[0].at.as_second(), 1_793_000_000);
        assert_eq!(log[0].subject, "Create JD-1: A b");
        assert_eq!(log[0].trailer("JoeDocs-Document"), Some("JD-1"));
        assert_eq!(log[0].trailer("JoeDocs-Client"), Some("claude code"));
        assert_eq!(log[0].trailer("JoeDocs-Action"), None);
        assert_eq!(
            log[0].changes,
            [(Change::Added, "p/plans/A b.md".to_owned())]
        );
        assert_eq!(log[1].id, second);
        assert_eq!(log[1].trailers, Vec::new());
        assert_eq!(
            log[1].changes,
            [
                (Change::Added, "other.md".to_owned()),
                (Change::Deleted, "p/plans/A b.md".to_owned()),
                (Change::Added, "p/plans/B.md".to_owned()),
            ]
        );
    }

    #[test]
    fn malformed_log_output_is_an_error() {
        assert_eq!(parse_log("").unwrap(), Vec::new());
        let short = parse_log("\x1eabc\0laptop").unwrap_err();
        assert!(short.to_string().contains("a short record"), "{short}");
        let time = parse_log("\x1eabc\0laptop\0soon\0s\0\0").unwrap_err();
        assert!(time.to_string().contains("author time"), "{time}");
        let status = parse_log("\x1eabc\0laptop\x001\0s\0\0\0R100\0a\0").unwrap_err();
        assert!(
            status.to_string().contains("change status R100"),
            "{status}"
        );
        let path = parse_log("\x1eabc\0laptop\x001\0s\0\0\0M\0").unwrap_err();
        assert!(path.to_string().contains("without a path"), "{path}");
        let ok = parse_log("\x1eabc\0laptop\x001\0s\0K: v\x1fbad\0\0\nM\0a.md\0").unwrap();
        assert_eq!(ok[0].trailers, [("K".to_owned(), "v".to_owned())]);
        assert_eq!(ok[0].changes, [(Change::Modified, "a.md".to_owned())]);
    }

    #[test]
    fn show_rejects_unknown_commits() {
        let repo = repo();
        assert!(matches!(
            repo.git.show("0000000000000000000000000000000000000000", "a.md"),
            Err(HistoryError::Missing(object)) if object.ends_with(":a.md")
        ));
    }
}
