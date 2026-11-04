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
    fn show_rejects_unknown_commits() {
        let repo = repo();
        assert!(matches!(
            repo.git.show("0000000000000000000000000000000000000000", "a.md"),
            Err(HistoryError::Missing(object)) if object.ends_with(":a.md")
        ));
    }
}
