//! The encrypted secret store.
//!
//! Collection files are meant to be committed. Credentials are not. Everything
//! sensitive lives here instead, referenced from requests as
//! `{{secret:name}}`, so that `git add .` is never a credential leak.
//!
//! The file is XChaCha20-Poly1305 over a JSON map. The key comes from the OS
//! keychain by default (a random 32-byte key, never derived from anything
//! guessable); a passphrase mode using Argon2id exists for headless machines
//! and CI, where there is no keychain to talk to.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use gabriel_core::vars::SecretProvider;
use rand::Rng as _;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const KEYCHAIN_SERVICE: &str = "dev.gabriel.vault";
const FORMAT_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("{path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path}: not a Gabriel vault ({message})")]
    Malformed { path: PathBuf, message: String },

    #[error("vault {path} is version {found}; this build understands version {FORMAT_VERSION}")]
    Version { path: PathBuf, found: u32 },

    #[error("could not decrypt the vault — wrong passphrase, or the keychain entry is gone")]
    Decrypt,

    #[error("OS keychain unavailable: {0}. Use a passphrase instead: GABRIEL_VAULT_PASSPHRASE=…")]
    Keychain(String),

    #[error("passphrase must be at least 8 characters")]
    WeakPassphrase,
}

type Result<T> = std::result::Result<T, VaultError>;

/// How the encryption key is obtained.
#[derive(Debug, Clone)]
pub enum KeySource {
    /// A random key held in the OS keychain, keyed by the vault's id.
    Keychain,
    Passphrase(String),
}

impl KeySource {
    /// Keychain normally; a passphrase when `GABRIEL_VAULT_PASSPHRASE` is set
    /// (CI, containers, and anywhere without a login keychain).
    pub fn from_environment() -> Self {
        match std::env::var("GABRIEL_VAULT_PASSPHRASE") {
            Ok(passphrase) if !passphrase.is_empty() => KeySource::Passphrase(passphrase),
            _ => KeySource::Keychain,
        }
    }
}

/// The encrypted envelope as it sits on disk. Only `ciphertext` holds secrets;
/// everything else is metadata needed to open it.
#[derive(Debug, Serialize, Deserialize)]
struct VaultFile {
    version: u32,
    id: String,
    kdf: Kdf,
    nonce: String,
    ciphertext: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Kdf {
    /// Key material lives in the OS keychain; nothing to derive.
    Keychain,
    Argon2id {
        salt: String,
        m_cost: u32,
        t_cost: u32,
        p_cost: u32,
    },
}

// Argon2id parameters: 64 MiB / 3 passes. Interactive-grade — a vault unlock
// should cost a person a blink and an attacker a fortune.
const ARGON_M_COST: u32 = 65_536;
const ARGON_T_COST: u32 = 3;
const ARGON_P_COST: u32 = 1;

pub struct Vault {
    path: PathBuf,
    id: String,
    key: [u8; 32],
    kdf: Kdf,
    secrets: BTreeMap<String, String>,
    dirty: bool,
}

impl Vault {
    /// Open an existing vault, or create an empty one at `path`.
    pub fn open(path: impl AsRef<Path>, source: &KeySource) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            return Self::create(path, source);
        }

        let text = std::fs::read_to_string(&path).map_err(|source| VaultError::Io {
            path: path.clone(),
            source,
        })?;
        let file: VaultFile = serde_json::from_str(&text).map_err(|e| VaultError::Malformed {
            path: path.clone(),
            message: e.to_string(),
        })?;

        if file.version != FORMAT_VERSION {
            return Err(VaultError::Version {
                path,
                found: file.version,
            });
        }

        let key = match (&file.kdf, source) {
            (Kdf::Keychain, _) => keychain_key(&file.id)?,
            (
                Kdf::Argon2id {
                    salt,
                    m_cost,
                    t_cost,
                    p_cost,
                },
                KeySource::Passphrase(pass),
            ) => {
                let salt = gabriel_core::b64_decode(salt).ok_or_else(|| VaultError::Malformed {
                    path: path.clone(),
                    message: "salt is not valid base64".into(),
                })?;
                derive_key(pass, &salt, *m_cost, *t_cost, *p_cost)?
            }
            (Kdf::Argon2id { .. }, KeySource::Keychain) => {
                return Err(VaultError::Keychain(
                    "this vault is passphrase-protected".to_string(),
                ));
            }
        };

        let nonce = gabriel_core::b64_decode(&file.nonce).ok_or_else(|| VaultError::Malformed {
            path: path.clone(),
            message: "nonce is not valid base64".into(),
        })?;
        let ciphertext =
            gabriel_core::b64_decode(&file.ciphertext).ok_or_else(|| VaultError::Malformed {
                path: path.clone(),
                message: "ciphertext is not valid base64".into(),
            })?;

        let nonce = XNonce::try_from(nonce.as_slice()).map_err(|_| VaultError::Malformed {
            path: path.clone(),
            message: "nonce is the wrong length".into(),
        })?;
        let plaintext = XChaCha20Poly1305::new_from_slice(&key)
            .map_err(|_| VaultError::Decrypt)?
            .decrypt(&nonce, ciphertext.as_ref())
            .map_err(|_| VaultError::Decrypt)?;

        let secrets: BTreeMap<String, String> =
            serde_json::from_slice(&plaintext).map_err(|e| VaultError::Malformed {
                path: path.clone(),
                message: e.to_string(),
            })?;

        Ok(Vault {
            path,
            id: file.id,
            key,
            kdf: file.kdf,
            secrets,
            dirty: false,
        })
    }

    fn create(path: PathBuf, source: &KeySource) -> Result<Self> {
        let id = format!("{:016x}", random_u64());
        let (key, kdf) = match source {
            KeySource::Keychain => (create_keychain_key(&id)?, Kdf::Keychain),
            KeySource::Passphrase(pass) => {
                if pass.len() < 8 {
                    return Err(VaultError::WeakPassphrase);
                }
                let mut salt = [0u8; 16];
                rand::rng().fill_bytes(&mut salt);
                let key = derive_key(pass, &salt, ARGON_M_COST, ARGON_T_COST, ARGON_P_COST)?;
                (
                    key,
                    Kdf::Argon2id {
                        salt: gabriel_core::b64_encode(&salt),
                        m_cost: ARGON_M_COST,
                        t_cost: ARGON_T_COST,
                        p_cost: ARGON_P_COST,
                    },
                )
            }
        };
        let mut vault = Vault {
            path,
            id,
            key,
            kdf,
            secrets: BTreeMap::new(),
            dirty: true,
        };
        vault.save()?;
        // Written to disk, so no longer pending: leaving the flag set makes
        // `save_if_dirty` rewrite an unchanged vault.
        vault.dirty = false;
        Ok(vault)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.secrets.get(name).map(String::as_str)
    }

    pub fn set(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.secrets.insert(name.into(), value.into());
        self.dirty = true;
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let removed = self.secrets.remove(name).is_some();
        self.dirty |= removed;
        removed
    }

    /// Secret *names* only. There is deliberately no API that dumps every value.
    pub fn names(&self) -> Vec<&str> {
        self.secrets.keys().map(String::as_str).collect()
    }

    pub fn len(&self) -> usize {
        self.secrets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty()
    }

    pub fn save(&self) -> Result<()> {
        let plaintext = serde_json::to_vec(&self.secrets).expect("string map serializes");

        let mut nonce = [0u8; 24];
        rand::rng().fill_bytes(&mut nonce);

        let ciphertext = XChaCha20Poly1305::new_from_slice(&self.key)
            .map_err(|_| VaultError::Decrypt)?
            .encrypt(&XNonce::from(nonce), plaintext.as_ref())
            .map_err(|_| VaultError::Decrypt)?;

        let file = VaultFile {
            version: FORMAT_VERSION,
            id: self.id.clone(),
            kdf: self.kdf.clone(),
            nonce: gabriel_core::b64_encode(&nonce),
            ciphertext: gabriel_core::b64_encode(&ciphertext),
        };

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| VaultError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let text = serde_json::to_string_pretty(&file).expect("envelope serializes");
        write_private(&self.path, text.as_bytes())
    }

    pub fn save_if_dirty(&mut self) -> Result<()> {
        if self.dirty {
            self.save()?;
            self.dirty = false;
        }
        Ok(())
    }
}

impl SecretProvider for Vault {
    fn secret(&self, key: &str) -> Option<String> {
        self.secrets.get(key).cloned()
    }
}

/// Write with `0600` where the platform has POSIX permissions. A vault that
/// every process on the machine can read is not a vault.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let io = |source| VaultError::Io {
        path: path.to_path_buf(),
        source,
    };

    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(io)?;
        file.write_all(bytes).map_err(io)?;
        file.sync_all().map_err(io)?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes).map_err(io)?;
    }
    Ok(())
}

fn derive_key(passphrase: &str, salt: &[u8], m: u32, t: u32, p: u32) -> Result<[u8; 32]> {
    let params = argon2::Params::new(m, t, p, Some(32)).map_err(|e| VaultError::Malformed {
        path: PathBuf::new(),
        message: e.to_string(),
    })?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut key = [0u8; 32];
    argon
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|_| VaultError::Decrypt)?;
    Ok(key)
}

fn keychain_key(id: &str) -> Result<[u8; 32]> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, id)
        .map_err(|e| VaultError::Keychain(e.to_string()))?;
    let encoded = entry
        .get_password()
        .map_err(|e| VaultError::Keychain(e.to_string()))?;
    let bytes = gabriel_core::b64_decode(&encoded).ok_or(VaultError::Decrypt)?;
    bytes.try_into().map_err(|_| VaultError::Decrypt)
}

fn create_keychain_key(id: &str) -> Result<[u8; 32]> {
    let mut key = [0u8; 32];
    rand::rng().fill_bytes(&mut key);
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, id)
        .map_err(|e| VaultError::Keychain(e.to_string()))?;
    entry
        .set_password(&gabriel_core::b64_encode(&key))
        .map_err(|e| VaultError::Keychain(e.to_string()))?;
    Ok(key)
}

fn random_u64() -> u64 {
    let mut bytes = [0u8; 8];
    rand::rng().fill_bytes(&mut bytes);
    u64::from_le_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gabriel-vault-test-{}", random_u64()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn passphrase() -> KeySource {
        KeySource::Passphrase("correct horse battery".to_string())
    }

    #[test]
    fn secrets_survive_a_close_and_reopen() {
        let path = temp_path("vault.json");
        {
            let mut vault = Vault::open(&path, &passphrase()).unwrap();
            vault.set("api_token", "sk-live-abc");
            vault.save().unwrap();
        }
        let vault = Vault::open(&path, &passphrase()).unwrap();
        assert_eq!(vault.get("api_token"), Some("sk-live-abc"));
    }

    #[test]
    fn the_file_on_disk_reveals_nothing() {
        let path = temp_path("vault.json");
        let mut vault = Vault::open(&path, &passphrase()).unwrap();
        vault.set("api_token", "sk-live-abc");
        vault.save().unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("sk-live-abc"), "plaintext on disk:\n{raw}");
        assert!(!raw.contains("api_token"), "secret names leak:\n{raw}");
    }

    #[test]
    fn the_wrong_passphrase_does_not_open_it() {
        let path = temp_path("vault.json");
        let mut vault = Vault::open(&path, &passphrase()).unwrap();
        vault.set("k", "v");
        vault.save().unwrap();

        let wrong = KeySource::Passphrase("wrong horse battery".to_string());
        assert!(matches!(
            Vault::open(&path, &wrong),
            Err(VaultError::Decrypt)
        ));
    }

    #[test]
    fn tampering_with_the_ciphertext_is_detected() {
        let path = temp_path("vault.json");
        let mut vault = Vault::open(&path, &passphrase()).unwrap();
        vault.set("k", "v");
        vault.save().unwrap();

        let mut file: VaultFile =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let mut bytes = gabriel_core::b64_decode(&file.ciphertext).unwrap();
        bytes[0] ^= 0xff;
        file.ciphertext = gabriel_core::b64_encode(&bytes);
        std::fs::write(&path, serde_json::to_string(&file).unwrap()).unwrap();

        assert!(matches!(
            Vault::open(&path, &passphrase()),
            Err(VaultError::Decrypt)
        ));
    }

    #[test]
    fn rejects_a_trivial_passphrase() {
        let path = temp_path("vault.json");
        let weak = KeySource::Passphrase("short".to_string());
        assert!(matches!(
            Vault::open(&path, &weak),
            Err(VaultError::WeakPassphrase)
        ));
    }

    #[test]
    fn acts_as_a_secret_provider() {
        let path = temp_path("vault.json");
        let mut vault = Vault::open(&path, &passphrase()).unwrap();
        vault.set("api_token", "sk-live-abc");

        let mut resolver = gabriel_core::vars::Resolver::new().with_secrets(&vault);
        assert_eq!(
            resolver.resolve("Bearer {{secret:api_token}}").unwrap(),
            "Bearer sk-live-abc"
        );
    }

    /// Which key source is chosen is a security decision, and it is made from an
    /// environment variable — so it deserves a test even though it is three
    /// lines long.
    #[test]
    fn the_key_source_follows_the_environment() {
        // Serialised against other tests that touch the same variable.
        static GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _held = GUARD.lock().unwrap_or_else(|e| e.into_inner());

        let previous = std::env::var("GABRIEL_VAULT_PASSPHRASE").ok();

        unsafe { std::env::remove_var("GABRIEL_VAULT_PASSPHRASE") };
        assert!(matches!(KeySource::from_environment(), KeySource::Keychain));

        unsafe { std::env::set_var("GABRIEL_VAULT_PASSPHRASE", "a-real-passphrase") };
        match KeySource::from_environment() {
            KeySource::Passphrase(p) => assert_eq!(p, "a-real-passphrase"),
            other => panic!("expected a passphrase, got {other:?}"),
        }

        // An empty value must not be mistaken for a passphrase.
        unsafe { std::env::set_var("GABRIEL_VAULT_PASSPHRASE", "") };
        assert!(matches!(KeySource::from_environment(), KeySource::Keychain));

        match previous {
            Some(value) => unsafe { std::env::set_var("GABRIEL_VAULT_PASSPHRASE", value) },
            None => unsafe { std::env::remove_var("GABRIEL_VAULT_PASSPHRASE") },
        }
    }

    #[test]
    fn saving_only_happens_when_something_changed() {
        let path = temp_path("vault.json");
        let mut vault = Vault::open(&path, &passphrase()).unwrap();
        let first = std::fs::read(&path).unwrap();

        // Nothing changed: the file must not be rewritten with a fresh nonce.
        vault.save_if_dirty().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), first);

        vault.set("k", "v");
        vault.save_if_dirty().unwrap();
        assert_ne!(
            std::fs::read(&path).unwrap(),
            first,
            "a change was not persisted"
        );
    }

    #[test]
    fn removing_and_listing_names() {
        let path = temp_path("vault.json");
        let mut vault = Vault::open(&path, &passphrase()).unwrap();
        vault.set("a", "1");
        vault.set("b", "2");
        assert_eq!(vault.names(), vec!["a", "b"]);
        assert!(vault.remove("a"));
        assert!(!vault.remove("a"));
        assert_eq!(vault.names(), vec!["b"]);
    }

    #[cfg(unix)]
    #[test]
    fn the_vault_file_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt as _;
        let path = temp_path("vault.json");
        let vault = Vault::open(&path, &passphrase()).unwrap();
        let mode = std::fs::metadata(vault.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "vault is readable by others: {mode:o}");
    }

    /// Touches the real login keychain, so it is opt-in rather than part of the
    /// default run.
    #[test]
    #[ignore = "requires an OS keychain"]
    fn keychain_mode_round_trips() {
        let path = temp_path("vault.json");
        {
            let mut vault = Vault::open(&path, &KeySource::Keychain).unwrap();
            vault.set("k", "v");
            vault.save().unwrap();
        }
        let vault = Vault::open(&path, &KeySource::Keychain).unwrap();
        assert_eq!(vault.get("k"), Some("v"));
    }
}
