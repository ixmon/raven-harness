//! Encrypted API key vault for inference endpoints.
//!
//! Uses AES-256-GCM for authenticated encryption with Argon2id for
//! password-based key derivation.  Keys are stored as base64-encoded
//! `nonce || ciphertext` blobs in `~/.raven-hotel/endpoints.json`.
//!
//! The vault password is only prompted when encrypted keys exist.

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{anyhow, bail, Context, Result};
use argon2::Argon2;
use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

enum KeyUpdate {
    Keep,
    Remove,
    Replace(String),
}

const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

/// On-disk representation of a saved endpoint.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredEndpoint {
    pub label: String,
    pub base_url: String,
    pub model: String,
    /// Base64-encoded `nonce || ciphertext`, or null if no key.
    pub encrypted_key: Option<String>,
    /// Convenience flag so we know whether to prompt for vault password.
    #[serde(default)]
    pub has_key: bool,
}

/// Launch-time defaults (settings list index 0). Persisted in endpoints.json.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct LaunchDefaults {
    pub label: String,
    pub base_url: String,
    pub model: String,
    pub encrypted_key: Option<String>,
    #[serde(default)]
    pub has_key: bool,
}

/// The full on-disk file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeystoreFile {
    /// Base64-encoded 16-byte salt for Argon2id.
    pub salt: String,
    pub endpoints: Vec<StoredEndpoint>,
    /// Overrides CLI/env defaults on next launch when present.
    #[serde(default)]
    pub launch: Option<LaunchDefaults>,
    /// Encrypted Brave Search API key (same nonce||ciphertext format as endpoint keys).
    #[serde(default)]
    pub brave_api_key: Option<String>,
}

/// Runtime key vault — holds the derived AES key in memory after unlock.
pub struct Keystore {
    path: PathBuf,
    file: KeystoreFile,
    derived_key: Option<[u8; KEY_LEN]>,
}

impl Keystore {
    /// Load an existing keystore or create an empty one.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        let file = if path.exists() {
            let data = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            serde_json::from_str::<KeystoreFile>(&data).with_context(|| "parsing endpoints.json")?
        } else {
            // Generate a fresh salt for future use
            let mut salt = [0u8; SALT_LEN];
            rand::thread_rng().fill_bytes(&mut salt);
            KeystoreFile {
                salt: base64::engine::general_purpose::STANDARD.encode(salt),
                endpoints: vec![],
                launch: None,
                brave_api_key: None,
            }
        };

        Ok(Self {
            path: path.to_path_buf(),
            file,
            derived_key: None,
        })
    }

    /// Returns true if any stored endpoint (or Brave key) has an encrypted API key.
    pub fn has_encrypted_keys(&self) -> bool {
        self.file.endpoints.iter().any(|e| e.has_key) || self.file.brave_api_key.is_some()
    }

    /// Derive the AES-256 key from a password + the stored salt.
    /// Returns Ok(()) if successful, Err if the password can't derive a key
    /// (shouldn't happen unless salt is corrupt).
    pub fn unlock(&mut self, password: &str) -> Result<()> {
        let salt = base64::engine::general_purpose::STANDARD
            .decode(&self.file.salt)
            .context("decoding salt")?;

        let mut key = [0u8; KEY_LEN];
        Argon2::default()
            .hash_password_into(password.as_bytes(), &salt, &mut key)
            .map_err(|e| anyhow!("argon2 key derivation failed: {}", e))?;

        // Verify by trying to decrypt the first encrypted key
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| anyhow!("cipher init: {}", e))?;

        for ep in &self.file.endpoints {
            if let Some(ref enc) = ep.encrypted_key {
                let blob = base64::engine::general_purpose::STANDARD
                    .decode(enc)
                    .context("decoding encrypted key")?;
                if blob.len() < NONCE_LEN {
                    bail!("encrypted key too short");
                }
                let nonce = Nonce::from_slice(&blob[..NONCE_LEN]);
                cipher
                    .decrypt(nonce, &blob[NONCE_LEN..])
                    .map_err(|_| anyhow!("wrong password or corrupted key"))?;
            }
        }

        self.derived_key = Some(key);
        Ok(())
    }

    /// Check if the keystore has been unlocked.
    pub fn is_unlocked(&self) -> bool {
        self.derived_key.is_some()
    }

    /// Encrypt a plaintext API key. Requires prior unlock().
    pub fn encrypt_key(&self, plaintext: &str) -> Result<String> {
        let key = self
            .derived_key
            .ok_or_else(|| anyhow!("keystore not unlocked"))?;
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| anyhow!("cipher init: {}", e))?;

        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| anyhow!("encryption failed: {}", e))?;

        // Store as nonce || ciphertext
        let mut blob = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        blob.extend_from_slice(&nonce_bytes);
        blob.extend_from_slice(&ciphertext);

        Ok(base64::engine::general_purpose::STANDARD.encode(&blob))
    }

    /// Decrypt a base64-encoded `nonce || ciphertext` blob.
    pub fn decrypt_key(&self, encoded: &str) -> Result<String> {
        let key = self
            .derived_key
            .ok_or_else(|| anyhow!("keystore not unlocked"))?;
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| anyhow!("cipher init: {}", e))?;

        let blob = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .context("decoding encrypted key")?;
        if blob.len() < NONCE_LEN {
            bail!("encrypted key too short");
        }

        let nonce = Nonce::from_slice(&blob[..NONCE_LEN]);
        let plaintext = cipher
            .decrypt(nonce, &blob[NONCE_LEN..])
            .map_err(|_| anyhow!("decryption failed — wrong password?"))?;

        String::from_utf8(plaintext).context("decrypted key is not valid UTF-8")
    }

    /// Set up the vault password for the first time (when adding the first key).
    pub fn init_password(&mut self, password: &str) -> Result<()> {
        // Generate fresh salt
        let mut salt = [0u8; SALT_LEN];
        rand::thread_rng().fill_bytes(&mut salt);
        self.file.salt = base64::engine::general_purpose::STANDARD.encode(salt);

        let mut key = [0u8; KEY_LEN];
        Argon2::default()
            .hash_password_into(password.as_bytes(), &salt, &mut key)
            .map_err(|e| anyhow!("argon2 key derivation failed: {}", e))?;

        self.derived_key = Some(key);
        Ok(())
    }

    /// Get all stored endpoints, decrypting API keys where present.
    pub fn decrypt_all_endpoints(&self) -> Result<Vec<raven_tui::config::InferenceEndpoint>> {
        let mut out = Vec::new();
        for ep in &self.file.endpoints {
            let api_key = if let Some(ref enc) = ep.encrypted_key {
                if self.derived_key.is_some() {
                    Some(self.decrypt_key(enc)?)
                } else {
                    None // keystore not unlocked, skip
                }
            } else {
                None
            };
            out.push(raven_tui::config::InferenceEndpoint {
                label: ep.label.clone(),
                base_url: ep.base_url.clone(),
                model: ep.model.clone(),
                api_key,
            });
        }
        Ok(out)
    }

    /// Add an endpoint and persist.
    pub fn add_endpoint(
        &mut self,
        label: &str,
        base_url: &str,
        model: &str,
        api_key: Option<&str>,
    ) -> Result<()> {
        let (encrypted_key, has_key) = if let Some(key) = api_key {
            if key.is_empty() {
                (None, false)
            } else {
                (Some(self.encrypt_key(key)?), true)
            }
        } else {
            (None, false)
        };

        self.file.endpoints.push(StoredEndpoint {
            label: label.to_string(),
            base_url: base_url.to_string(),
            model: model.to_string(),
            encrypted_key,
            has_key,
        });

        self.save()
    }

    /// Remove an endpoint by index and persist.
    pub fn remove_endpoint(&mut self, idx: usize) -> Result<()> {
        if idx < self.file.endpoints.len() {
            self.file.endpoints.remove(idx);
            self.save()?;
        }
        Ok(())
    }

    /// Launch defaults for the session endpoint (settings index 0).
    pub fn launch_endpoint(&self) -> Result<Option<raven_tui::config::InferenceEndpoint>> {
        let Some(launch) = &self.file.launch else {
            return Ok(None);
        };
        let api_key = if let Some(ref enc) = launch.encrypted_key {
            if self.derived_key.is_some() {
                Some(self.decrypt_key(enc)?)
            } else {
                None
            }
        } else {
            None
        };
        Ok(Some(raven_tui::config::InferenceEndpoint {
            label: launch.label.clone(),
            base_url: launch.base_url.clone(),
            model: launch.model.clone(),
            api_key,
        }))
    }

    /// Set the launch endpoint from an InferenceEndpoint (used when switching endpoints in settings).
    pub fn set_launch_endpoint(&mut self, ep: &raven_tui::config::InferenceEndpoint) -> Result<()> {
        let api_key = ep.api_key.as_deref();

        let key_update = match api_key {
            None => KeyUpdate::Keep,
            Some("") => KeyUpdate::Remove,
            Some(key) => KeyUpdate::Replace(self.encrypt_key(key)?),
        };

        let mut launch = self.file.launch.take().unwrap_or_default();
        launch.label = ep.label.clone();
        launch.base_url = ep.base_url.clone();
        launch.model = ep.model.clone();

        match key_update {
            KeyUpdate::Keep => {}
            KeyUpdate::Remove => {
                launch.encrypted_key = None;
                launch.has_key = false;
            }
            KeyUpdate::Replace(encrypted) => {
                launch.encrypted_key = Some(encrypted);
                launch.has_key = true;
            }
        }

        self.file.launch = Some(launch);
        self.save()
    }

    /// Persist launch defaults (settings edit on index 0).
    pub fn set_launch_defaults(
        &mut self,
        label: &str,
        base_url: &str,
        model: &str,
        api_key: Option<&str>,
    ) -> Result<()> {
        let key_update = match api_key {
            None => KeyUpdate::Keep,
            Some("") => KeyUpdate::Remove,
            Some(key) => KeyUpdate::Replace(self.encrypt_key(key)?),
        };

        let mut launch = self.file.launch.take().unwrap_or_default();
        launch.label = label.to_string();
        launch.base_url = base_url.to_string();
        launch.model = model.to_string();

        match key_update {
            KeyUpdate::Keep => {}
            KeyUpdate::Remove => {
                launch.encrypted_key = None;
                launch.has_key = false;
            }
            KeyUpdate::Replace(encrypted) => {
                launch.encrypted_key = Some(encrypted);
                launch.has_key = true;
            }
        }

        self.file.launch = Some(launch);
        self.save()
    }

    /// Update a stored endpoint in place. `api_key`: `None` keeps the existing key,
    /// `Some("")` removes it, `Some("key")` replaces it.
    pub fn update_endpoint(
        &mut self,
        idx: usize,
        label: &str,
        base_url: &str,
        model: &str,
        api_key: Option<&str>,
    ) -> Result<()> {
        if idx >= self.file.endpoints.len() {
            bail!("endpoint index {} out of range", idx);
        }

        let key_update = match api_key {
            None => KeyUpdate::Keep,
            Some("") => KeyUpdate::Remove,
            Some(key) => KeyUpdate::Replace(self.encrypt_key(key)?),
        };

        let ep = &mut self.file.endpoints[idx];
        ep.label = label.to_string();
        ep.base_url = base_url.to_string();
        ep.model = model.to_string();

        match key_update {
            KeyUpdate::Keep => {}
            KeyUpdate::Remove => {
                ep.encrypted_key = None;
                ep.has_key = false;
            }
            KeyUpdate::Replace(encrypted) => {
                ep.encrypted_key = Some(encrypted);
                ep.has_key = true;
            }
        }

        self.save()
    }

    /// Number of stored endpoints.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.file.endpoints.len()
    }

    /// Get stored endpoint labels for display.
    #[allow(dead_code)]
    pub fn stored_endpoints(&self) -> &[StoredEndpoint] {
        &self.file.endpoints
    }

    /// Write the keystore to disk.
    fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(&self.file)?;
        std::fs::write(&self.path, data)
            .with_context(|| format!("writing {}", self.path.display()))?;
        Ok(())
    }

    /// Store an encrypted Brave Search API key.
    pub fn set_brave_key(&mut self, key: &str) -> Result<()> {
        if key.is_empty() {
            return self.clear_brave_key();
        }
        let encrypted = self.encrypt_key(key)?;
        self.file.brave_api_key = Some(encrypted);
        self.save()
    }

    /// Retrieve the decrypted Brave API key, or fall back to BRAVE_API_KEY env var.
    pub fn get_brave_key(&self) -> Option<String> {
        // Keystore takes precedence
        if let Some(ref enc) = self.file.brave_api_key {
            if self.derived_key.is_some() {
                if let Ok(key) = self.decrypt_key(enc) {
                    return Some(key);
                }
            }
        }
        // Fall back to environment variable
        std::env::var("BRAVE_API_KEY").ok().filter(|s| !s.is_empty())
    }

    /// Remove the stored Brave API key.
    pub fn clear_brave_key(&mut self) -> Result<()> {
        self.file.brave_api_key = None;
        self.save()
    }

    /// Check if a Brave API key is available (stored or env).
    pub fn has_brave_key(&self) -> bool {
        self.file.brave_api_key.is_some()
            || std::env::var("BRAVE_API_KEY").ok().filter(|s| !s.is_empty()).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn unique_test_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "raven_keystore_test_{}_{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn cleanup(path: &Path) {
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn keystore_init_add_roundtrip_and_wrong_password() {
        let path = unique_test_path();
        cleanup(&path);

        let mut ks = Keystore::load_or_create(&path).expect("load");
        assert!(!ks.is_unlocked());
        assert!(!ks.has_encrypted_keys());

        // First time setup
        ks.init_password("test-password-123").expect("init pw");

        // Add an endpoint with a secret key
        ks.add_endpoint(
            "test-ep",
            "https://example.com/v1",
            "gpt-test",
            Some("sk-secret-key-xyz"),
        )
        .expect("add endpoint with key");

        assert!(ks.has_encrypted_keys());
        assert!(ks.is_unlocked());

        // Decrypt should give back the key
        let eps = ks.decrypt_all_endpoints().expect("decrypt all");
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].label, "test-ep");
        assert_eq!(eps[0].api_key.as_deref(), Some("sk-secret-key-xyz"));

        // Reload from disk and unlock with correct pw
        let mut ks2 = Keystore::load_or_create(&path).expect("reload");
        ks2.unlock("test-password-123")
            .expect("correct password unlock");
        let eps2 = ks2.decrypt_all_endpoints().expect("decrypt after reload");
        assert_eq!(eps2[0].api_key.as_deref(), Some("sk-secret-key-xyz"));

        // Wrong password should fail
        let mut ks3 = Keystore::load_or_create(&path).expect("reload2");
        let wrong = ks3.unlock("wrong-password");
        assert!(wrong.is_err(), "wrong password must be rejected");

        cleanup(&path);
    }

    #[test]
    fn keystore_update_endpoint_persists() {
        let path = unique_test_path();
        cleanup(&path);

        let mut ks = Keystore::load_or_create(&path).unwrap();
        ks.init_password("pw").unwrap();
        ks.add_endpoint("ep1", "http://one/v1", "model-a", None)
            .unwrap();

        ks.update_endpoint(0, "ep1-renamed", "http://two/v1", "model-b", None)
            .unwrap();

        let ks2 = Keystore::load_or_create(&path).unwrap();
        assert_eq!(ks2.file.endpoints[0].label, "ep1-renamed");
        assert_eq!(ks2.file.endpoints[0].base_url, "http://two/v1");
        assert_eq!(ks2.file.endpoints[0].model, "model-b");

        cleanup(&path);
    }

    #[test]
    fn keystore_launch_defaults_persist() {
        let path = unique_test_path();
        cleanup(&path);

        let mut ks = Keystore::load_or_create(&path).unwrap();
        ks.set_launch_defaults("local", "http://127.0.0.1:9090/v1", "qwen", None)
            .unwrap();

        let ks2 = Keystore::load_or_create(&path).unwrap();
        let launch = ks2.launch_endpoint().unwrap().expect("launch");
        assert_eq!(launch.label, "local");
        assert_eq!(launch.base_url, "http://127.0.0.1:9090/v1");
        assert_eq!(launch.model, "qwen");

        cleanup(&path);
    }

    #[test]
    fn keystore_add_without_key_and_remove() {
        let path = unique_test_path();
        cleanup(&path);

        let mut ks = Keystore::load_or_create(&path).unwrap();
        ks.init_password("pw").unwrap();

        ks.add_endpoint("no-key-ep", "http://local", "model", None)
            .unwrap();
        assert!(!ks.has_encrypted_keys());

        let eps = ks.decrypt_all_endpoints().unwrap();
        assert!(eps[0].api_key.is_none());

        ks.remove_endpoint(0).unwrap();
        assert_eq!(ks.len(), 0);

        cleanup(&path);
    }

    #[test]
    fn keystore_brave_key_roundtrip() {
        let path = unique_test_path();
        cleanup(&path);

        let mut ks = Keystore::load_or_create(&path).unwrap();
        ks.init_password("pw").unwrap();

        // No key yet
        assert!(!ks.has_brave_key());
        assert!(ks.get_brave_key().is_none());

        // Set key
        ks.set_brave_key("BSAtest123456").unwrap();
        assert!(ks.has_brave_key());
        assert_eq!(ks.get_brave_key().as_deref(), Some("BSAtest123456"));

        // Reload from disk
        let mut ks2 = Keystore::load_or_create(&path).unwrap();
        ks2.unlock("pw").unwrap();
        assert_eq!(ks2.get_brave_key().as_deref(), Some("BSAtest123456"));

        // Clear key
        ks2.clear_brave_key().unwrap();
        assert!(ks2.get_brave_key().is_none());
        // After clear, file field is None
        assert!(ks2.file.brave_api_key.is_none());

        cleanup(&path);
    }


}

