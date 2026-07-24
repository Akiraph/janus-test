use std::{fs::OpenOptions, io::Write, path::Path};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use rand::RngCore;
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};

use crate::config::RunMode;

pub struct Secret(SecretString);

impl Secret {
    pub fn new(value: String) -> Self {
        Self(SecretString::from(value))
    }

    pub fn expose(&self) -> &str {
        self.0.expose_secret()
    }
}

#[derive(Clone)]
pub struct SecretCipher {
    key: [u8; 32],
}

impl SecretCipher {
    pub fn load(data_root: &Path, mode: RunMode) -> anyhow::Result<Self> {
        let key = if let Ok(encoded) = std::env::var("JANUS_MASTER_KEY") {
            decode_key(&encoded)?
        } else if mode == RunMode::Development {
            load_or_create_development_key(data_root)?
        } else {
            anyhow::bail!("JANUS_MASTER_KEY is required in production");
        };
        Ok(Self { key })
    }

    pub fn encrypt(&self, plaintext: &Secret, associated_data: &str) -> anyhow::Result<Vec<u8>> {
        let cipher = XChaCha20Poly1305::new((&self.key).into());
        let mut nonce = [0_u8; 24];
        rand::rng().fill_bytes(&mut nonce);
        let encrypted = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext.expose().as_bytes(),
                    aad: associated_data.as_bytes(),
                },
            )
            .map_err(|_| anyhow::anyhow!("encrypt secret"))?;
        let mut stored = Vec::with_capacity(nonce.len() + encrypted.len());
        stored.extend_from_slice(&nonce);
        stored.extend_from_slice(&encrypted);
        Ok(stored)
    }

    pub fn decrypt(&self, stored: &[u8], associated_data: &str) -> anyhow::Result<Secret> {
        let (nonce, ciphertext) = stored
            .split_at_checked(24)
            .ok_or_else(|| anyhow::anyhow!("invalid encrypted secret"))?;
        let cipher = XChaCha20Poly1305::new((&self.key).into());
        let plaintext = cipher
            .decrypt(
                XNonce::from_slice(nonce),
                Payload {
                    msg: ciphertext,
                    aad: associated_data.as_bytes(),
                },
            )
            .map_err(|_| anyhow::anyhow!("decrypt secret"))?;
        Ok(Secret::new(String::from_utf8(plaintext)?))
    }
}

pub fn random_token(bytes: usize) -> String {
    let mut value = vec![0_u8; bytes];
    rand::rng().fill_bytes(&mut value);
    URL_SAFE_NO_PAD.encode(value)
}

pub fn purpose_hash(purpose: &str, value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"janus\0");
    hasher.update(purpose.as_bytes());
    hasher.update(b"\0");
    hasher.update(value.as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

pub fn fingerprint(value: &str) -> String {
    purpose_hash("secret-fingerprint", value)[..12].to_owned()
}

/// Mask an API key for display: fully masked when ≤8 chars, otherwise the
/// first 4 and last 4 chars are visible with 8 `*` between them
/// (e.g. `sk-r********-key`). Matches Janus-old `maskApiKey`.
pub fn mask_key(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() <= 8 {
        return "*".repeat(trimmed.len());
    }
    let head = &trimmed[..4];
    let tail = &trimmed[trimmed.len() - 4..];
    format!("{head}{}{tail}", "*".repeat(8))
}

fn decode_key(encoded: &str) -> anyhow::Result<[u8; 32]> {
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| anyhow::anyhow!("JANUS_MASTER_KEY must be base64url without padding"))?;
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("JANUS_MASTER_KEY must decode to exactly 32 bytes"))
}

fn load_or_create_development_key(data_root: &Path) -> anyhow::Result<[u8; 32]> {
    let path = data_root.join("development-master.key");
    match std::fs::read_to_string(&path) {
        Ok(value) => decode_key(value.trim()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut key = [0_u8; 32];
            rand::rng().fill_bytes(&mut key);
            let encoded = URL_SAFE_NO_PAD.encode(key);
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            file.write_all(encoded.as_bytes())?;
            Ok(key)
        }
        Err(error) => Err(error.into()),
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Secret([REDACTED])")
    }
}

#[cfg(test)]
mod tests {
    use super::{Secret, SecretCipher, mask_key};

    #[test]
    fn debug_is_redacted() {
        let secret = Secret::new("never-print-this".into());
        assert_eq!(format!("{secret:?}"), "Secret([REDACTED])");
        assert_eq!(secret.expose(), "never-print-this");
    }

    #[test]
    fn cipher_binds_associated_data() {
        let cipher = SecretCipher { key: [7; 32] };
        let stored = cipher
            .encrypt(&Secret::new("sensitive".into()), "tenant/provider/key")
            .expect("encryption succeeds");
        assert_eq!(
            cipher
                .decrypt(&stored, "tenant/provider/key")
                .expect("decryption succeeds")
                .expose(),
            "sensitive"
        );
        assert!(cipher.decrypt(&stored, "other/provider/key").is_err());
    }

    #[test]
    fn mask_key_hides_short_values_fully() {
        assert_eq!(mask_key("abc"), "***");
        assert_eq!(mask_key("12345678"), "********");
        assert_eq!(mask_key("  short  "), "*****");
    }

    #[test]
    fn mask_key_exposes_head_and_tail_for_long_values() {
        assert_eq!(mask_key("sk-real-key"), "sk-r********-key");
        assert_eq!(mask_key("sk-ant-api03-longtoken-xyz"), "sk-a********-xyz");
        assert_eq!(mask_key("  sk-real-key  "), "sk-r********-key");
    }
}
