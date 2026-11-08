//! Time-based one-time passwords (RFC 6238) for admin logins, as Google
//! Authenticator and other TOTP apps generate them: HMAC-SHA1, 6 digits, a
//! 30-second step. Also the base32 encoding (RFC 4648) and the `otpauth://`
//! URI that apps use for enrolment.

use hmac::{Hmac, KeyInit as _, Mac as _};
use sha1::Sha1;

/// Seconds per TOTP step.
pub const STEP_SECONDS: i64 = 30;
/// Digits in a code.
pub const DIGITS: u32 = 6;
/// Steps accepted on either side of the current one, for clock skew.
pub const SKEW_STEPS: i64 = 1;
/// The issuer shown in authenticator apps.
pub const ISSUER: &str = "JoeDocs";

const BASE32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// The HOTP value (RFC 4226) of `secret` at `counter`, with `digits` digits.
#[must_use]
pub fn hotp(secret: &[u8], counter: u64, digits: u32) -> u32 {
    let Ok(mut mac) = Hmac::<Sha1>::new_from_slice(secret) else {
        return 0;
    };
    mac.update(&counter.to_be_bytes());
    let hash = mac.finalize().into_bytes();
    let offset = usize::from(hash[hash.len() - 1] & 0x0f);
    let binary = u32::from_be_bytes([
        hash[offset] & 0x7f,
        hash[offset + 1],
        hash[offset + 2],
        hash[offset + 3],
    ]);
    binary % 10_u32.pow(digits)
}

/// The TOTP step that `unix_seconds` falls in.
#[must_use]
pub fn step_at(unix_seconds: i64) -> i64 {
    unix_seconds.div_euclid(STEP_SECONDS)
}

/// The 6-digit code for `step`, zero-padded.
#[must_use]
pub fn code(secret: &[u8], step: i64) -> String {
    let counter = u64::try_from(step).unwrap_or(0);
    format!("{:06}", hotp(secret, counter, DIGITS))
}

/// The step that `code` is valid for, if it matches the step of
/// `unix_seconds` or one within [`SKEW_STEPS`], and is later than
/// `last_used` (so a code is never accepted twice).
#[must_use]
pub fn verify(
    secret: &[u8],
    code_given: &str,
    unix_seconds: i64,
    last_used: Option<i64>,
) -> Option<i64> {
    let given = code_given.trim();
    if given.len() != 6 || !given.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let now = step_at(unix_seconds);
    (now - SKEW_STEPS..=now + SKEW_STEPS)
        .filter(|step| last_used.is_none_or(|last| *step > last))
        .find(|step| constant_time_eq(code(secret, *step).as_bytes(), given.as_bytes()))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0_u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// RFC 4648 base32, without `=` padding (as authenticator apps expect).
#[must_use]
pub fn base32_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    for chunk in bytes.chunks(5) {
        let mut buf = [0_u8; 5];
        buf[..chunk.len()].copy_from_slice(chunk);
        let bits = u64::from_be_bytes([0, 0, 0, buf[0], buf[1], buf[2], buf[3], buf[4]]);
        let chars = (chunk.len() * 8).div_ceil(5);
        for index in 0..chars {
            let shift = 35 - index * 5;
            out.push(char::from(
                BASE32[usize::try_from((bits >> shift) & 0x1f).unwrap_or(0)],
            ));
        }
    }
    out
}

/// Decodes RFC 4648 base32 (padding, spaces and case are ignored).
#[must_use]
pub fn base32_decode(text: &str) -> Option<Vec<u8>> {
    let mut bits: u64 = 0;
    let mut count = 0;
    let mut out = Vec::new();
    for c in text.chars().filter(|c| !c.is_whitespace() && *c != '=') {
        let value = BASE32
            .iter()
            .position(|b| char::from(*b) == c.to_ascii_uppercase())?;
        bits = (bits << 5) | u64::try_from(value).ok()?;
        count += 5;
        if count >= 8 {
            count -= 8;
            out.push(u8::try_from((bits >> count) & 0xff).ok()?);
        }
    }
    Some(out)
}

/// The enrolment URI for an admin token named `name` (a slug, so it needs
/// no escaping).
#[must_use]
pub fn otpauth_uri(name: &str, secret_base32: &str) -> String {
    format!(
        "otpauth://totp/{ISSUER}:{name}?secret={secret_base32}&issuer={ISSUER}&algorithm=SHA1&digits={DIGITS}&period={STEP_SECONDS}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4226 Appendix D: the secret is ASCII "12345678901234567890".
    const RFC_SECRET: &[u8] = b"12345678901234567890";

    #[test]
    fn hotp_matches_rfc_4226_appendix_d() {
        let expected = [
            755_224, 287_082, 359_152, 969_429, 338_314, 254_676, 287_922, 162_583, 399_871,
            520_489,
        ];
        for (counter, value) in expected.iter().enumerate() {
            assert_eq!(
                hotp(RFC_SECRET, counter as u64, 6),
                *value,
                "counter {counter}"
            );
        }
    }

    #[test]
    fn totp_matches_rfc_6238_appendix_b_sha1() {
        for (time, value) in [
            (59_i64, 94_287_082),
            (1_111_111_109, 7_081_804),
            (1_111_111_111, 14_050_471),
            (1_234_567_890, 89_005_924),
            (2_000_000_000, 69_279_037),
            (20_000_000_000, 65_353_130),
        ] {
            let step = u64::try_from(step_at(time)).unwrap();
            assert_eq!(hotp(RFC_SECRET, step, 8), value, "time {time}");
        }
        assert_eq!(code(RFC_SECRET, step_at(59)), "287082");
    }

    #[test]
    fn verify_accepts_one_step_of_skew_once() {
        let now = 1_111_111_111;
        let step = step_at(now);
        for offset in [-1, 0, 1] {
            let given = code(RFC_SECRET, step + offset);
            assert_eq!(verify(RFC_SECRET, &given, now, None), Some(step + offset));
            // The same step, or an earlier one, is never accepted again.
            assert_eq!(verify(RFC_SECRET, &given, now, Some(step + offset)), None);
        }
        for offset in [-2, 2] {
            assert_eq!(
                verify(RFC_SECRET, &code(RFC_SECRET, step + offset), now, None),
                None
            );
        }
        assert_eq!(verify(RFC_SECRET, "12345", now, None), None);
        assert_eq!(verify(RFC_SECRET, "12345a", now, None), None);
        assert_eq!(
            verify(
                RFC_SECRET,
                &format!(" {} ", code(RFC_SECRET, step)),
                now,
                None
            ),
            Some(step)
        );
    }

    #[test]
    fn base32_matches_rfc_4648_section_10() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "MY"),
            ("fo", "MZXQ"),
            ("foo", "MZXW6"),
            ("foob", "MZXW6YQ"),
            ("fooba", "MZXW6YTB"),
            ("foobar", "MZXW6YTBOI"),
        ] {
            assert_eq!(base32_encode(plain.as_bytes()), encoded);
            assert_eq!(base32_decode(encoded).unwrap(), plain.as_bytes());
        }
        assert_eq!(base32_decode("mzxw6ytboi======").unwrap(), b"foobar");
        assert_eq!(base32_decode("MZ1"), None);
    }

    #[test]
    fn the_uri_names_the_issuer_and_parameters() {
        assert_eq!(
            otpauth_uri("laptop", "ABC"),
            "otpauth://totp/JoeDocs:laptop?secret=ABC&issuer=JoeDocs&algorithm=SHA1&digits=6&period=30"
        );
    }
}
