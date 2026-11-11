//! `PlantUML`'s text encoding for diagram URLs: the diagram text, compressed
//! with raw DEFLATE, then written in `PlantUML`'s own 64-character alphabet
//! (`0-9`, `A-Z`, `a-z`, `-`, `_`). A server draws the diagram from
//! `<server>/svg/<encoded>`.

use std::io::Write as _;

use flate2::Compression;
use flate2::write::DeflateEncoder;

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
