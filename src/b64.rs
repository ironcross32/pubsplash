//! Standard base64, encode and decode.
//!
//! Small enough not to be worth a dependency, and needed in three unrelated
//! places: the Icecast `Authorization` header, DPAPI-protected credentials in
//! the settings file, and Google Cloud's JSON-wrapped audio.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Decodes standard base64, tolerating whitespace and missing padding.
///
/// Also accepts the URL-safe alphabet: some services hand back `-` and `_`,
/// and rejecting those would be a confusing failure for no benefit.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let mut bits = 0u32;
    let mut count = 0u32;
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    for ch in text.bytes() {
        if ch == b'=' || ch.is_ascii_whitespace() {
            continue;
        }
        let ch = match ch {
            b'-' => b'+',
            b'_' => b'/',
            other => other,
        };
        let value = ALPHABET.iter().position(|c| *c == ch)? as u32;
        bits = bits << 6 | value;
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The RFC 4648 test vectors.
    #[test]
    fn matches_the_rfc_vectors() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(encode(plain.as_bytes()), encoded, "encoding {plain:?}");
            assert_eq!(
                decode(encoded).as_deref(),
                Some(plain.as_bytes()),
                "decoding {encoded:?}"
            );
        }
    }

    #[test]
    fn decoding_tolerates_whitespace_and_absent_padding() {
        assert_eq!(decode("Zm9v YmFy").unwrap(), b"foobar");
        assert_eq!(decode("Zm9vYg").unwrap(), b"foob");
        assert_eq!(decode("Zm9v\nYmFy").unwrap(), b"foobar");
    }

    #[test]
    fn the_url_safe_alphabet_decodes_too() {
        // 0xfb 0xff encodes as "+/" in standard and "-_" in URL-safe.
        assert_eq!(decode("-_8=").unwrap(), decode("+/8=").unwrap());
    }

    #[test]
    fn characters_outside_the_alphabet_are_rejected() {
        assert!(decode("Zm9v!").is_none());
        assert!(decode("**").is_none());
    }

    #[test]
    fn arbitrary_bytes_round_trip() {
        let bytes: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        assert_eq!(decode(&encode(&bytes)).unwrap(), bytes);
    }
}
