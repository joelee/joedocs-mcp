//! `PlantUML` in the web interface: how diagrams are drawn ([`Diagrams`]),
//! and `PlantUML`'s text encoding for diagram URLs: the diagram text,
//! compressed with raw DEFLATE, then written in `PlantUML`'s own
//! 64-character alphabet (`0-9`, `A-Z`, `a-z`, `-`, `_`). A server draws
//! the diagram from `<server>/svg/<encoded>`.

use std::io::Write as _;
use std::path::{Path, PathBuf};

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
