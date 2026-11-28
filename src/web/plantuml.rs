//! `PlantUML` in the web interface: how diagrams are drawn ([`Diagrams`]),
//! and `PlantUML`'s text encoding for diagram URLs: the diagram text,
//! compressed with raw DEFLATE, then written in `PlantUML`'s own
//! 64-character alphabet (`0-9`, `A-Z`, `a-z`, `-`, `_`). A server draws
//! the diagram from `<server>/svg/<encoded>`.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use sha1::{Digest as _, Sha1};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use flate2::Compression;
use flate2::write::DeflateEncoder;

/// How the web interface shows `PlantUML` diagrams, chosen when the server
/// starts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Diagrams {
    /// Drawn on the server by this `plantuml` command
    /// (`admin.use_plantuml_cli`, with the command on `PATH`).
    Cli(PathBuf),
    /// Drawn by this `PlantUML` server, which the browser asks
    /// (`admin.web_plantuml_url`).
    Server(String),
    /// Shown as source.
    #[default]
    Source,
}

impl Diagrams {
    /// The mode for `admin.use_plantuml_cli` and `admin.web_plantuml_url`:
    /// the `plantuml` on `PATH` (read through `env`) when the key is on and
    /// one is found, else the server when its URL is set, else source. With
    /// the key on and no command found, also a warning for the log.
    #[must_use]
    pub fn choose(
        use_cli: bool,
        server_url: &str,
        env: &dyn crate::env::Env,
        fs: &dyn crate::fs::FileSystem,
    ) -> (Self, Option<String>) {
        if use_cli && let Some(path) = find_on_path("plantuml", env, fs) {
            return (Self::Cli(path), None);
        }
        let fallback = Self::server_or_source(server_url);
        let warning = use_cli.then(|| {
            format!(
                "admin.use_plantuml_cli is on, but no plantuml command is on PATH; PlantUML diagrams are {}",
                fallback.describe()
            )
        });
        (fallback, warning)
    }

    /// The server at `server_url`, or source when it is empty.
    #[must_use]
    pub fn server_or_source(server_url: &str) -> Self {
        if server_url.is_empty() {
            Self::Source
        } else {
            Self::Server(server_url.to_owned())
        }
    }

    /// The server URL to draw from, or `""` when diagrams are not drawn by
    /// a server.
    #[must_use]
    pub fn server_url(&self) -> &str {
        match self {
            Self::Server(url) => url,
            Self::Cli(_) | Self::Source => "",
        }
    }

    /// The mode in words, for `joedocs status`.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Cli(path) => format!("drawn on the server by {}", path.display()),
            Self::Server(url) => format!("drawn by {url}"),
            Self::Source => "shown as source".to_owned(),
        }
    }
}

/// How long one drawing may take before the command is killed.
pub const DRAW_TIMEOUT: Duration = Duration::from_secs(10);
/// How many drawings run at once.
pub const DRAW_SLOTS: usize = 2;
/// The largest SVG the command may write, in bytes.
pub const MAX_SVG_BYTES: usize = 8 * 1024 * 1024;
/// The `PATH` the command runs with: its own directory, then these.
const COMMAND_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// A `PlantUML` block of a rendered page: its cache key and the source the
/// command draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagram {
    /// [`key`] of `source`.
    pub key: String,
    /// The block's text, wrapped in `@startuml` and `@enduml` when it has
    /// no `@start` line.
    pub source: String,
}

impl Diagram {
    /// The diagram of a block's text.
    #[must_use]
    pub fn of(text: &str) -> Self {
        let source = if text.trim_start().starts_with("@start") {
            text.to_owned()
        } else if text.ends_with('\n') {
            format!("@startuml\n{text}@enduml\n")
        } else {
            format!("@startuml\n{text}\n@enduml\n")
        };
        Self {
            key: key(&source),
            source,
        }
    }
}

/// The cache key of a diagram's source: its SHA-1, in lower-case hex.
#[must_use]
pub fn key(source: &str) -> String {
    crate::auth::hex(&Sha1::digest(source.as_bytes()))
}

/// Whether `text` is a cache key: 40 lower-case hex digits.
#[must_use]
pub fn is_key(text: &str) -> bool {
    text.len() == 40
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The image cache, `<state_dir>/cache/plantuml/`: `<key>.puml` holds a
/// diagram's source and `<key>.svg` its drawing. It is not backed up or
/// checked, and deleting it is safe: it fills again as pages are viewed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    /// The cache of the state directory `state_dir`.
    #[must_use]
    pub fn new(state_dir: &Path) -> Self {
        Self {
            dir: state_dir.join("cache").join("plantuml"),
        }
    }

    /// The cache directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn file(&self, key: &str, extension: &str) -> PathBuf {
        self.dir.join(format!("{key}.{extension}"))
    }

    /// Writes the source of each diagram the cache does not have yet,
    /// creating the directory (`0700`) when needed.
    ///
    /// # Errors
    ///
    /// Returns the I/O error.
    pub fn save_sources(&self, diagrams: &[Diagram]) -> std::io::Result<()> {
        use std::os::unix::fs::DirBuilderExt as _;
        if diagrams.is_empty() {
            return Ok(());
        }
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)?;
        for diagram in diagrams {
            let path = self.file(&diagram.key, "puml");
            if !path.exists() {
                crate::admin::write_atomically(&path, diagram.source.as_bytes())?;
            }
        }
        Ok(())
    }

    /// The source saved under `key`, if any.
    #[must_use]
    pub fn source(&self, key: &str) -> Option<String> {
        std::fs::read_to_string(self.file(key, "puml")).ok()
    }

    /// The drawing saved under `key`, if any.
    #[must_use]
    pub fn svg(&self, key: &str) -> Option<Vec<u8>> {
        std::fs::read(self.file(key, "svg")).ok()
    }

    /// Saves the drawing of `key`.
    ///
    /// # Errors
    ///
    /// Returns the I/O error.
    pub fn save_svg(&self, key: &str, svg: &[u8]) -> std::io::Result<()> {
        crate::admin::write_atomically(&self.file(key, "svg"), svg)
    }
}

/// Why a drawing failed.
#[derive(Debug, thiserror::Error)]
pub enum DrawError {
    /// The command could not be started or read.
    #[error("cannot run plantuml: {0}")]
    Io(#[from] std::io::Error),
    /// It ran past the time limit and was killed.
    #[error("plantuml ran for more than {0:?}")]
    Timeout(Duration),
    /// It wrote nothing.
    #[error("plantuml wrote no image")]
    Empty,
    /// It wrote more than [`MAX_SVG_BYTES`].
    #[error("plantuml wrote more than {MAX_SVG_BYTES} bytes")]
    TooLarge,
}

/// The command that draws one diagram: `<cli> -tsvg -pipe`, without a
/// shell, with a cleared environment holding only `PATH`, `HOME` (the
/// cache directory) and `PLANTUML_SECURITY_PROFILE=SANDBOX`, which refuses
/// `!include` and other file or URL access. It is killed when dropped.
#[must_use]
pub fn command(cli: &Path, cache: &Cache) -> tokio::process::Command {
    let own = cli
        .parent()
        .map(|dir| format!("{}:", dir.display()))
        .unwrap_or_default();
    let mut command = tokio::process::Command::new(cli);
    command
        .args(["-tsvg", "-pipe"])
        .env_clear()
        .env("PATH", format!("{own}{COMMAND_PATH}"))
        .env("HOME", cache.dir())
        .env("PLANTUML_SECURITY_PROFILE", "SANDBOX")
        .current_dir(cache.dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command
}

/// Draws `source` with `cli`: its SVG, also when it exits non-zero after
/// writing one (`PlantUML`'s error image).
///
/// # Errors
///
/// Returns [`DrawError`] when it cannot run, takes longer than `timeout`
/// (it is then killed), writes nothing, or writes more than
/// [`MAX_SVG_BYTES`].
pub async fn draw(
    cli: &Path,
    cache: &Cache,
    source: &str,
    timeout: Duration,
) -> Result<Vec<u8>, DrawError> {
    let mut child = command(cli, cache).spawn()?;
    let run = async {
        let mut stdin = child.stdin.take().ok_or(DrawError::Empty)?;
        let mut stdout = child.stdout.take().ok_or(DrawError::Empty)?;
        let source = source.as_bytes().to_vec();
        let feed = async move {
            // A command that stops reading early is not an error here: what
            // it wrote decides.
            let _ = stdin.write_all(&source).await;
            drop(stdin);
        };
        let mut svg = Vec::new();
        let limit = u64::try_from(MAX_SVG_BYTES).unwrap_or(u64::MAX) + 1;
        let mut limited = (&mut stdout).take(limit);
        let read = limited.read_to_end(&mut svg);
        let ((), read) = tokio::join!(feed, read);
        read?;
        if svg.len() > MAX_SVG_BYTES {
            return Err(DrawError::TooLarge);
        }
        child.wait().await?;
        if svg.is_empty() {
            return Err(DrawError::Empty);
        }
        Ok(svg)
    };
    match tokio::time::timeout(timeout, run).await {
        Ok(result) => result,
        Err(_) => Err(DrawError::Timeout(timeout)),
    }
}

/// The first executable `name` in the absolute directories of `PATH`.
fn find_on_path(
    name: &str,
    env: &dyn crate::env::Env,
    fs: &dyn crate::fs::FileSystem,
) -> Option<PathBuf> {
    let path = env.non_empty("PATH")?;
    path.split(':')
        .map(Path::new)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| fs.is_executable(candidate))
}

/// The longest encoded diagram put in a URL. Longer ones are shown as
/// source: many servers and proxies refuse URLs much over 8 KiB.
pub const MAX_ENCODED: usize = 6000;

const ALPHABET: &[u8; 64] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz-_";

/// `text` encoded for a `PlantUML` server URL, or `None` when it is longer
/// than [`MAX_ENCODED`] or cannot be compressed.
#[must_use]
pub fn encode(text: &str) -> Option<String> {
    let mut deflate = DeflateEncoder::new(Vec::new(), Compression::best());
    deflate.write_all(text.as_bytes()).ok()?;
    let compressed = deflate.finish().ok()?;
    let encoded = encode64(&compressed);
    (encoded.len() <= MAX_ENCODED).then_some(encoded)
}

/// `bytes` in `PlantUML`'s alphabet, 3 bytes to 4 characters; a short last
/// group is padded with zero bytes, as `PlantUML` does.
fn encode64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for group in bytes.chunks(3) {
        let b0 = group[0];
        let b1 = group.get(1).copied().unwrap_or(0);
        let b2 = group.get(2).copied().unwrap_or(0);
        for six in [
            b0 >> 2,
            ((b0 & 0x3) << 4) | (b1 >> 4),
            ((b1 & 0xF) << 2) | (b2 >> 6),
            b2 & 0x3F,
        ] {
            out.push(char::from(ALPHABET[usize::from(six)]));
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Read as _;

    use super::*;

    /// The inverse of [`encode64`], for the round trip.
    fn decode64(text: &str) -> Vec<u8> {
        let six = |c: u8| u8::try_from(ALPHABET.iter().position(|a| *a == c).unwrap()).unwrap();
        let mut out = Vec::new();
        for group in text.as_bytes().chunks(4) {
            let [a, b, c, d] = [six(group[0]), six(group[1]), six(group[2]), six(group[3])];
            out.extend([(a << 2) | (b >> 4), (b << 4) | (c >> 2), (c << 6) | d]);
        }
        out
    }

    use crate::env::MapEnv;
    use crate::fs::MemFs;

    #[test]
    fn the_cli_is_used_when_on_and_found_on_path() {
        let env = MapEnv::new().with("PATH", "/opt/none:/usr/local/bin:/usr/bin");
        let fs = MemFs::new()
            .with("/usr/local/bin/plantuml", "")
            .with_executable("/usr/bin/plantuml");
        // Not executable in /usr/local/bin: the next directory wins.
        let (mode, warning) = Diagrams::choose(true, "https://p.example.net", &env, &fs);
        assert_eq!(mode, Diagrams::Cli("/usr/bin/plantuml".into()));
        assert_eq!(warning, None);
        assert_eq!(mode.describe(), "drawn on the server by /usr/bin/plantuml");
        assert_eq!(mode.server_url(), "");
    }

    #[test]
    fn a_missing_cli_falls_back_with_a_warning() {
        let env = MapEnv::new().with("PATH", "/usr/bin::relative");
        let fs = MemFs::new().with_executable("relative/plantuml");
        let (mode, warning) = Diagrams::choose(true, "https://p.example.net/plantuml", &env, &fs);
        assert_eq!(
            mode,
            Diagrams::Server("https://p.example.net/plantuml".into())
        );
        assert_eq!(mode.server_url(), "https://p.example.net/plantuml");
        let warning = warning.unwrap();
        assert!(warning.contains("admin.use_plantuml_cli"), "{warning}");
        assert!(warning.contains("no plantuml"), "{warning}");
        let (mode, warning) = Diagrams::choose(true, "", &MapEnv::new(), &fs);
        assert_eq!(mode, Diagrams::Source);
        assert!(warning.is_some());
    }

    #[test]
    fn without_the_key_the_server_or_source_is_used() {
        let env = MapEnv::new().with("PATH", "/usr/bin");
        let fs = MemFs::new().with_executable("/usr/bin/plantuml");
        assert_eq!(
            Diagrams::choose(false, "https://p.example.net", &env, &fs),
            (Diagrams::Server("https://p.example.net".into()), None)
        );
        let (mode, warning) = Diagrams::choose(false, "", &env, &fs);
        assert_eq!((&mode, warning), (&Diagrams::Source, None));
        assert_eq!(mode.describe(), "shown as source");
        assert_eq!(
            Diagrams::Server("https://p.example.net".into()).describe(),
            "drawn by https://p.example.net"
        );
    }

    #[test]
    fn a_diagram_is_keyed_by_the_sha1_of_its_source() {
        let diagram = Diagram::of("@startuml\nA -> B\n@enduml\n");
        assert_eq!(diagram.source, "@startuml\nA -> B\n@enduml\n");
        // A SHA-1 in lower-case hex: 40 characters.
        assert_eq!(diagram.key.len(), 40);
        assert!(
            diagram
                .key
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert!(is_key(&diagram.key));
        assert_eq!(Diagram::of("@startuml\nA -> B\n@enduml\n").key, diagram.key);
        assert_eq!(Diagram::of("A -> B").source, "@startuml\nA -> B\n@enduml\n");
        assert_eq!(Diagram::of("A -> B").key, diagram.key);
        assert_ne!(Diagram::of("A -> C\n").key, diagram.key);
        let upper = diagram.key.to_ascii_uppercase();
        for bad in ["", upper.as_str(), "../x", &diagram.key[..8]] {
            assert!(!is_key(bad), "{bad}");
        }
    }

    #[test]
    fn the_command_has_no_shell_and_a_cleared_sandboxed_environment() {
        let cache = Cache::new(Path::new("/state"));
        assert_eq!(cache.dir(), Path::new("/state/cache/plantuml"));
        let command = command(Path::new("/opt/pu/bin/plantuml"), &cache);
        let std = command.as_std();
        assert_eq!(std.get_program(), "/opt/pu/bin/plantuml");
        let args: Vec<_> = std.get_args().collect();
        assert_eq!(args, ["-tsvg", "-pipe"]);
        let envs: Vec<(String, Option<String>)> = std
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert_eq!(
            envs,
            [
                ("HOME".to_owned(), Some("/state/cache/plantuml".to_owned())),
                (
                    "PATH".to_owned(),
                    Some("/opt/pu/bin:/usr/local/bin:/usr/bin:/bin".to_owned())
                ),
                (
                    "PLANTUML_SECURITY_PROFILE".to_owned(),
                    Some("SANDBOX".to_owned())
                ),
            ]
        );
        assert_eq!(
            std.get_current_dir(),
            Some(Path::new("/state/cache/plantuml"))
        );
        assert_eq!(DRAW_TIMEOUT, Duration::from_secs(10));
        assert_eq!((DRAW_SLOTS, MAX_SVG_BYTES), (2, 8 * 1024 * 1024));
    }

    #[test]
    fn sources_are_saved_once_in_a_private_directory() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path());
        cache.save_sources(&[]).unwrap();
        assert!(!cache.dir().exists(), "nothing to save, no directory");
        let diagram = Diagram::of("A -> B\n");
        cache.save_sources(std::slice::from_ref(&diagram)).unwrap();
        let mode = std::fs::metadata(cache.dir()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        assert_eq!(cache.source(&diagram.key).unwrap(), diagram.source);
        assert_eq!(cache.svg(&diagram.key), None);
        cache.save_svg(&diagram.key, b"<svg/>").unwrap();
        assert_eq!(cache.svg(&diagram.key).unwrap(), b"<svg/>");
    }

    #[test]
    fn the_alphabet_encodes_fixed_bytes() {
        assert_eq!(encode64(&[0, 0, 0]), "0000");
        assert_eq!(encode64(&[255, 255, 255]), "____");
        assert_eq!(encode64(&[0x04, 0x20, 0xC4]), "1234");
        assert_eq!(encode64(b"Man"), "JM5k");
        // A short last group is padded with zeros.
        assert_eq!(encode64(&[255]), "_m00");
        assert_eq!(encode64(&[]), "");
    }

    #[test]
    fn a_diagram_round_trips_through_inflate() {
        let text = "@startuml\nBob -> Alice : hello\n@enduml\n";
        let encoded = encode(text).unwrap();
        assert!(
            encoded
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "{encoded}"
        );
        let mut inflated = String::new();
        flate2::read::DeflateDecoder::new(decode64(&encoded).as_slice())
            .read_to_string(&mut inflated)
            .unwrap();
        // Zero padding may add bytes after the end of the stream; the
        // decoder stops at the end block.
        assert_eq!(inflated, text);
    }

    /// A diagram of 4000 different lines, which compresses badly.
    pub(crate) fn long_diagram() -> String {
        use std::fmt::Write as _;
        let mut lines = String::new();
        for i in 0..4000_u64 {
            let _ = writeln!(
                lines,
                "A{} -> B{} : m{i}",
                i * 7919 % 10007,
                i * 104_729 % 9973
            );
        }
        lines
    }

    #[test]
    fn diagrams_too_long_for_a_url_are_not_encoded() {
        let lines = long_diagram();
        assert!(encode(&lines).is_none());
    }
}
