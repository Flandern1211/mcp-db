use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rand::{rngs::OsRng, RngCore};
use zeroize::Zeroizing;

use crate::error::{Error, Result};

const MAGIC: &[u8; 6] = b"MCPDB1";
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

pub trait CredentialResolver: Send + Sync {
    fn resolve(&self, reference: &str) -> Result<String>;
}

/// Encrypted JSON map of `credential_ref -> DSN`, keyed by an externally supplied master key.
pub struct EncryptedFileResolver {
    entries: BTreeMap<String, String>,
}

impl EncryptedFileResolver {
    pub fn open(store_path: &Path, key_path: &Path) -> Result<Self> {
        let key = fs::read_to_string(key_path).map_err(|_| Error::CredentialUnavailable)?;
        let key = parse_key(&key).ok_or(Error::CredentialUnavailable)?;
        let data = fs::read(store_path).map_err(|_| Error::CredentialUnavailable)?;
        if data.len() < MAGIC.len() + NONCE_LEN || &data[..MAGIC.len()] != MAGIC {
            return Err(Error::CredentialUnavailable);
        }
        let nonce = &data[MAGIC.len()..MAGIC.len() + NONCE_LEN];
        let ciphertext = &data[MAGIC.len() + NONCE_LEN..];
        let cipher =
            ChaCha20Poly1305::new_from_slice(&key[..]).map_err(|_| Error::CredentialUnavailable)?;
        let plaintext = cipher
            .decrypt(Nonce::from_slice(nonce), ciphertext)
            .map_err(|_| Error::CredentialUnavailable)?;
        let entries =
            serde_json::from_slice(&plaintext).map_err(|_| Error::CredentialUnavailable)?;
        Ok(Self { entries })
    }

    /// Seal a credential map for tests and future admin commands.
    pub fn seal(
        store_path: &Path,
        key: &[u8; KEY_LEN],
        entries: &BTreeMap<String, String>,
    ) -> Result<()> {
        let plaintext = serde_json::to_vec(entries)
            .map_err(|_| Error::InvalidInput("cannot serialize credential store".to_string()))?;
        let cipher = ChaCha20Poly1305::new_from_slice(key)
            .map_err(|_| Error::InvalidInput("invalid credential key".to_string()))?;
        let mut nonce = [0_u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce);
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce), plaintext.as_ref())
            .map_err(|_| Error::InvalidInput("cannot encrypt credential store".to_string()))?;
        let mut data = Vec::with_capacity(MAGIC.len() + NONCE_LEN + ciphertext.len());
        data.extend_from_slice(MAGIC);
        data.extend_from_slice(&nonce);
        data.extend_from_slice(&ciphertext);

        if let Some(parent) = store_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|_| {
                    Error::InvalidInput("cannot create credential directory".to_string())
                })?;
            }
        }
        fs::write(store_path, data)
            .map_err(|_| Error::InvalidInput("cannot write credential store".to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(store_path, fs::Permissions::from_mode(0o600))
                .map_err(|_| Error::InvalidInput("cannot secure credential store".to_string()))?;
        }
        Ok(())
    }
}

impl CredentialResolver for EncryptedFileResolver {
    fn resolve(&self, reference: &str) -> Result<String> {
        self.entries
            .get(reference)
            .cloned()
            .ok_or(Error::CredentialUnavailable)
    }
}

fn parse_key(text: &str) -> Option<Zeroizing<[u8; KEY_LEN]>> {
    let trimmed = text.trim();
    if trimmed.len() == KEY_LEN * 2 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        let mut key = [0_u8; KEY_LEN];
        for (index, chunk) in trimmed.as_bytes().chunks(2).enumerate() {
            let pair = std::str::from_utf8(chunk).ok()?;
            key[index] = u8::from_str_radix(pair, 16).ok()?;
        }
        return Some(Zeroizing::new(key));
    }
    if let Ok(bytes) = STANDARD.decode(trimmed) {
        if bytes.len() == KEY_LEN {
            let mut key = [0_u8; KEY_LEN];
            key.copy_from_slice(&bytes);
            return Some(Zeroizing::new(key));
        }
    }
    if let Ok(bytes) = URL_SAFE_NO_PAD.decode(trimmed) {
        if bytes.len() == KEY_LEN {
            let mut key = [0_u8; KEY_LEN];
            key.copy_from_slice(&bytes);
            return Some(Zeroizing::new(key));
        }
    }
    None
}
