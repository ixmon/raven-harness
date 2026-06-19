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

/// The full on-disk file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeystoreFile {
    /// Base64-encoded 16-byte salt for Argon2id.
    pub salt: String,
    pub endpoints: Vec<StoredEndpoint>,
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
            serde_json::from_str::<KeystoreFile>(&data)
                .with_context(|| "parsing endpoints.json")?
        } else {
            // Generate a fresh salt for future use
            let mut salt = [0u8; SALT_LEN];
            rand::thread_rng().fill_bytes(&mut salt);
            KeystoreFile {
                salt: base64::engine::general_purpose::STANDARD.encode(salt),
                endpoints: vec![],
            }
        };

        Ok(Self {
            path: path.to_path_buf(),
            file,
            derived_key: None,
        })
    }

    /// Returns true if any stored endpoint has an encrypted API key.
    pub fn has_encrypted_keys(&self) -> bool {
        self.file.endpoints.iter().any(|e| e.has_key)
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
        let cipher = Aes256Gcm::new_from_slice(&key)
            .map_err(|e| anyhow!("cipher init: {}", e))?;

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
        let cipher = Aes256Gcm::new_from_slice(&key)
            .map_err(|e| anyhow!("cipher init: {}", e))?;

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
        let cipher = Aes256Gcm::new_from_slice(&key)
            .map_err(|e| anyhow!("cipher init: {}", e))?;

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
    pub fn decrypt_all_endpoints(&self) -> Result<Vec<crate::config::InferenceEndpoint>> {
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
            out.push(crate::config::InferenceEndpoint {
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
}
