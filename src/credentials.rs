use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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

/// Writes a credential reference. Provisioning is the only first-version writer:
/// the service owns the file as a single writer and rewrites it atomically.
pub trait CredentialSink: Send + Sync {
    fn store(&self, reference: &str, dsn: &str) -> Result<()>;
}

/// Encrypted JSON map of `credential_ref -> DSN`, keyed by an externally supplied master key.
pub struct EncryptedFileResolver {
    store_path: PathBuf,
    key: Zeroizing<[u8; KEY_LEN]>,
    entries: Mutex<BTreeMap<String, String>>,
}

impl EncryptedFileResolver {
    pub fn open(store_path: &Path, key_path: &Path) -> Result<Self> {
        let key = fs::read_to_string(key_path).map_err(|_| Error::CredentialUnavailable)?;
        let key = parse_key(&key).ok_or(Error::CredentialUnavailable)?;
        let entries = read_encrypted(store_path, &key)?;
        Ok(Self {
            store_path: store_path.to_path_buf(),
            key,
            entries: Mutex::new(entries),
        })
    }

    /// Seal a credential map for tests and future admin commands.
    pub fn seal(
        store_path: &Path,
        key: &[u8; KEY_LEN],
        entries: &BTreeMap<String, String>,
    ) -> Result<()> {
        write_encrypted(store_path, key, entries)
    }
}

impl CredentialResolver for EncryptedFileResolver {
    fn resolve(&self, reference: &str) -> Result<String> {
        self.entries
            .lock()
            .map_err(|_| Error::CredentialUnavailable)?
            .get(reference)
            .cloned()
            .ok_or(Error::CredentialUnavailable)
    }
}

impl CredentialSink for EncryptedFileResolver {
    fn store(&self, reference: &str, dsn: &str) -> Result<()> {
        crate::state::model::validate_credential_ref(reference)?;
        if dsn.trim().is_empty() || dsn.contains(['\n', '\r']) {
            return Err(Error::InvalidInput(
                "credential DSN must be a single non-empty line".to_string(),
            ));
        }
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| Error::InvalidInput("credential store is unavailable".to_string()))?;
        let mut next = entries.clone();
        next.insert(reference.to_string(), dsn.to_string());
        // Persist before exposing the new value to concurrent readers.
        write_encrypted(&self.store_path, &self.key, &next)?;
        *entries = next;
        Ok(())
    }
}

fn read_encrypted(path: &Path, key: &[u8; KEY_LEN]) -> Result<BTreeMap<String, String>> {
    let data = fs::read(path).map_err(|_| Error::CredentialUnavailable)?;
    if data.len() < MAGIC.len() + NONCE_LEN || &data[..MAGIC.len()] != MAGIC {
        return Err(Error::CredentialUnavailable);
    }
    let nonce = &data[MAGIC.len()..MAGIC.len() + NONCE_LEN];
    let ciphertext = &data[MAGIC.len() + NONCE_LEN..];
    let cipher = ChaCha20Poly1305::new_from_slice(key).map_err(|_| Error::CredentialUnavailable)?;
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| Error::CredentialUnavailable)?;
    serde_json::from_slice(&plaintext).map_err(|_| Error::CredentialUnavailable)
}

fn write_encrypted(
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
    let temporary = store_path.with_extension("tmp");
    {
        let mut file = File::create(&temporary)
            .map_err(|_| Error::InvalidInput("cannot write credential store".to_string()))?;
        file.write_all(&data)
            .map_err(|_| Error::InvalidInput("cannot write credential store".to_string()))?;
        file.sync_all()
            .map_err(|_| Error::InvalidInput("cannot flush credential store".to_string()))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))
            .map_err(|_| Error::InvalidInput("cannot secure credential store".to_string()))?;
    }
    fs::rename(&temporary, store_path).map_err(|_| {
        Error::InvalidInput("cannot atomically replace credential store".to_string())
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(store_path, fs::Permissions::from_mode(0o600))
            .map_err(|_| Error::InvalidInput("cannot secure credential store".to_string()))?;
    }
    Ok(())
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
