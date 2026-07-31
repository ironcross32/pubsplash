//! User-scoped encryption for credentials kept in the settings file.
//!
//! Speech engines need API keys, and a plaintext `config.json` is a key anyone
//! who copies the file can use. DPAPI ties the ciphertext to the Windows user
//! account, so a copied settings file is inert on another machine — which is
//! the realistic threat here, not a determined local attacker (Pubsplash runs
//! as the user and can always decrypt its own secrets).
//!
//! [`Secret`] is the serde-facing type. It writes `enc:<base64>` and reads
//! either that or a bare string, so settings written by an older build — or
//! hand-edited by a user — still load.

use crate::b64;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use windows::Win32::Foundation::LocalFree;
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};

/// Marks a value this module encrypted, so a plaintext value written by an
/// older build is distinguishable from ciphertext.
const PREFIX: &str = "enc:";

/// A credential that is encrypted at rest.
///
/// `Debug` is implemented by hand: these end up inside `Config`, which is
/// logged and dumped in a few places, and a derived `Debug` would put every
/// API key in the log file.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.is_empty() {
            f.write_str("Secret(empty)")
        } else {
            f.write_str("Secret(set)")
        }
    }
}

impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.0.is_empty() {
            return serializer.serialize_str("");
        }
        match protect(self.0.as_bytes()) {
            Some(bytes) => serializer.serialize_str(&format!("{PREFIX}{}", b64::encode(&bytes))),
            // Losing the user's typed key on a DPAPI failure would be worse
            // than storing it as-is; the next load reads it back as plaintext.
            None => {
                log::warn!("Could not encrypt a stored credential; writing it unencrypted");
                serializer.serialize_str(&self.0)
            }
        }
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        let Some(encoded) = raw.strip_prefix(PREFIX) else {
            // Plaintext from an older build, or hand-edited. Take it; the next
            // save re-writes it encrypted.
            return Ok(Self(raw));
        };
        let plain = b64::decode(encoded)
            .and_then(|bytes| unprotect(&bytes))
            .and_then(|bytes| String::from_utf8(bytes).ok());
        match plain {
            Some(value) => Ok(Self(value)),
            None => {
                // Encrypted under a different Windows account, or corrupt.
                // Returning an error here would fail the whole settings load.
                log::warn!("A stored credential could not be decrypted; treating it as unset");
                Ok(Self::default())
            }
        }
    }
}

fn protect(plain: &[u8]) -> Option<Vec<u8>> {
    unsafe {
        let input = CRYPT_INTEGER_BLOB {
            cbData: plain.len() as u32,
            pbData: plain.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB::default();
        CryptProtectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .ok()?;
        Some(take_blob(&mut output))
    }
}

fn unprotect(cipher: &[u8]) -> Option<Vec<u8>> {
    unsafe {
        let input = CRYPT_INTEGER_BLOB {
            cbData: cipher.len() as u32,
            pbData: cipher.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB::default();
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .ok()?;
        Some(take_blob(&mut output))
    }
}

/// Copies a blob DPAPI allocated and frees it. DPAPI hands back `LocalAlloc`
/// memory that the caller owns.
unsafe fn take_blob(blob: &mut CRYPT_INTEGER_BLOB) -> Vec<u8> {
    unsafe {
        let bytes = std::slice::from_raw_parts(blob.pbData, blob.cbData as usize).to_vec();
        let _ = LocalFree(Some(windows::Win32::Foundation::HLOCAL(
            blob.pbData as *mut _,
        )));
        blob.pbData = std::ptr::null_mut();
        blob.cbData = 0;
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_round_trips_through_json() {
        let secret = Secret::new("sk-a-test-api-key");
        let json = serde_json::to_string(&secret).unwrap();
        assert!(
            json.contains(PREFIX) && !json.contains("sk-a-test-api-key"),
            "the key leaked into the serialized form: {json}"
        );
        let back: Secret = serde_json::from_str(&json).unwrap();
        assert_eq!(back.as_str(), "sk-a-test-api-key");
    }

    /// Settings files written before credentials were encrypted must still load.
    #[test]
    fn plaintext_values_still_load() {
        let back: Secret = serde_json::from_str("\"plain-key\"").unwrap();
        assert_eq!(back.as_str(), "plain-key");
    }

    /// A key encrypted by another Windows account must not fail the whole load.
    #[test]
    fn undecryptable_values_read_as_unset() {
        let back: Secret = serde_json::from_str("\"enc:bm90LWEtcmVhbC1ibG9i\"").unwrap();
        assert!(back.is_empty());
    }

    #[test]
    fn empty_secrets_stay_empty_and_unencrypted() {
        let json = serde_json::to_string(&Secret::default()).unwrap();
        assert_eq!(json, "\"\"");
    }

    #[test]
    fn debug_never_prints_the_value() {
        let rendered = format!("{:?}", Secret::new("sk-secret"));
        assert!(!rendered.contains("sk-secret"), "{rendered}");
    }
}
