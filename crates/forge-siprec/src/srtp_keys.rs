//! SRTP Key Management for SIPREC
//!
//! This module handles extraction and forwarding of SRTP keys for secure
//! call recording. Keys can be extracted from SDP crypto attributes or
//! DTLS-SRTP key material.

use base64::{engine::general_purpose::STANDARD, Engine};
use dashmap::DashMap;
use forge_rtp::{SrtpKeyMaterial, SrtpProfile};
use std::sync::Arc;
use thiserror::Error;

/// SRTP key management error types
#[derive(Debug, Error)]
pub enum SrtpKeyError {
    /// Invalid crypto attribute
    #[error("Invalid crypto attribute: {0}")]
    InvalidCrypto(String),

    /// Invalid key material
    #[error("Invalid key material: {0}")]
    InvalidKeyMaterial(String),

    /// Key not found
    #[error("Key not found for stream: {0}")]
    KeyNotFound(String),

    /// Base64 decode error
    #[error("Base64 decode error: {0}")]
    Base64Error(String),

    /// SRTP error
    #[error("SRTP error: {0}")]
    Srtp(#[from] forge_core::ForgeError),
}

pub type Result<T> = std::result::Result<T, SrtpKeyError>;

/// SRTP key information for a stream
#[derive(Debug, Clone)]
pub struct StreamKeyInfo {
    /// Stream identifier
    pub stream_id: String,

    /// SSRC for this stream
    pub ssrc: u32,

    /// SRTP key material
    pub key_material: SrtpKeyMaterial,

    /// Tag (from a=crypto line)
    pub tag: u32,

    /// Crypto suite name
    pub suite: String,
}

/// SRTP Key Manager
///
/// Manages SRTP keys for multiple streams in a recording session.
#[derive(Debug, Clone)]
pub struct SrtpKeyManager {
    /// Keys indexed by stream ID
    keys_by_stream: Arc<DashMap<String, StreamKeyInfo>>,

    /// Keys indexed by SSRC
    keys_by_ssrc: Arc<DashMap<u32, String>>, // SSRC -> stream_id
}

impl SrtpKeyManager {
    /// Create a new SRTP key manager
    pub fn new() -> Self {
        Self {
            keys_by_stream: Arc::new(DashMap::new()),
            keys_by_ssrc: Arc::new(DashMap::new()),
        }
    }

    /// Add SRTP key material for a stream
    ///
    /// # Arguments
    ///
    /// * `stream_id` - Unique stream identifier
    /// * `ssrc` - RTP SSRC for this stream
    /// * `key_material` - SRTP key material (master key + salt)
    /// * `tag` - Crypto tag from SDP
    /// * `suite` - Crypto suite name
    pub fn add_key(
        &self,
        stream_id: String,
        ssrc: u32,
        key_material: SrtpKeyMaterial,
        tag: u32,
        suite: String,
    ) {
        let info = StreamKeyInfo {
            stream_id: stream_id.clone(),
            ssrc,
            key_material,
            tag,
            suite,
        };

        self.keys_by_stream.insert(stream_id.clone(), info);
        self.keys_by_ssrc.insert(ssrc, stream_id);
    }

    /// Get key information by stream ID
    pub fn get_key_by_stream(&self, stream_id: &str) -> Option<StreamKeyInfo> {
        self.keys_by_stream
            .get(stream_id)
            .map(|entry| entry.clone())
    }

    /// Get key information by SSRC
    pub fn get_key_by_ssrc(&self, ssrc: u32) -> Option<StreamKeyInfo> {
        if let Some(stream_id) = self.keys_by_ssrc.get(&ssrc) {
            self.get_key_by_stream(&stream_id)
        } else {
            None
        }
    }

    /// Remove key for a stream
    pub fn remove_key(&self, stream_id: &str) {
        if let Some((_, info)) = self.keys_by_stream.remove(stream_id) {
            self.keys_by_ssrc.remove(&info.ssrc);
        }
    }

    /// Get all stream IDs with keys
    pub fn get_all_stream_ids(&self) -> Vec<String> {
        self.keys_by_stream
            .iter()
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Parse SDP crypto attribute (a=crypto)
    ///
    /// Format: a=crypto:<tag> <crypto-suite> inline:<key>|<lifetime>|<mki>
    /// Example: a=crypto:1 AES_CM_128_HMAC_SHA1_80 inline:WVNfX19zZW1jdGwgKCkgewkyMjA7fQp9CnVubGVz
    pub fn parse_crypto_attribute(crypto_line: &str) -> Result<(u32, String, Vec<u8>, Vec<u8>)> {
        // Remove "a=crypto:" prefix if present
        let line = crypto_line.trim_start_matches("a=crypto:").trim();

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 {
            return Err(SrtpKeyError::InvalidCrypto(
                "Not enough parts in crypto line".to_string(),
            ));
        }

        // Parse tag
        let tag: u32 = parts[0]
            .parse()
            .map_err(|_| SrtpKeyError::InvalidCrypto("Invalid tag".to_string()))?;

        // Parse crypto suite
        let suite = parts[1].to_string();

        // Parse inline key material
        let inline_part = parts[2];
        if !inline_part.starts_with("inline:") {
            return Err(SrtpKeyError::InvalidCrypto(
                "Missing 'inline:' prefix".to_string(),
            ));
        }

        let key_param = inline_part.trim_start_matches("inline:");

        // Key may have |lifetime|mki suffix, remove it
        let key_b64 = key_param.split('|').next().unwrap();

        // Decode base64 key material
        let key_material = STANDARD
            .decode(key_b64)
            .map_err(|e| SrtpKeyError::Base64Error(format!("Failed to decode key: {}", e)))?;

        // Split into master key and master salt based on crypto suite
        let (master_key, master_salt) = match suite.as_str() {
            "AES_CM_128_HMAC_SHA1_80" | "AES_CM_128_HMAC_SHA1_32" => {
                // 16 bytes key + 14 bytes salt
                if key_material.len() < 30 {
                    return Err(SrtpKeyError::InvalidKeyMaterial(format!(
                        "Key material too short: {}",
                        key_material.len()
                    )));
                }
                (key_material[0..16].to_vec(), key_material[16..30].to_vec())
            }
            "AEAD_AES_128_GCM" => {
                // 16 bytes key + 12 bytes salt
                if key_material.len() < 28 {
                    return Err(SrtpKeyError::InvalidKeyMaterial(format!(
                        "Key material too short: {}",
                        key_material.len()
                    )));
                }
                (key_material[0..16].to_vec(), key_material[16..28].to_vec())
            }
            "AEAD_AES_256_GCM" => {
                // 32 bytes key + 12 bytes salt
                if key_material.len() < 44 {
                    return Err(SrtpKeyError::InvalidKeyMaterial(format!(
                        "Key material too short: {}",
                        key_material.len()
                    )));
                }
                (key_material[0..32].to_vec(), key_material[32..44].to_vec())
            }
            _ => {
                return Err(SrtpKeyError::InvalidCrypto(format!(
                    "Unsupported crypto suite: {}",
                    suite
                )))
            }
        };

        Ok((tag, suite, master_key, master_salt))
    }

    /// Generate SDP crypto attribute from key material
    ///
    /// Creates an a=crypto line for SDP
    pub fn generate_crypto_attribute(
        tag: u32,
        suite: &str,
        master_key: &[u8],
        master_salt: &[u8],
    ) -> String {
        // Concatenate key and salt
        let mut key_material = Vec::new();
        key_material.extend_from_slice(master_key);
        key_material.extend_from_slice(master_salt);

        // Encode to base64
        let key_b64 = STANDARD.encode(&key_material);

        format!("a=crypto:{} {} inline:{}", tag, suite, key_b64)
    }

    /// Convert crypto suite name to SRTP profile
    pub fn suite_to_profile(suite: &str) -> Result<SrtpProfile> {
        match suite {
            "AES_CM_128_HMAC_SHA1_80" => Ok(SrtpProfile::Aes128CmHmacSha1_80),
            "AES_CM_128_HMAC_SHA1_32" => Ok(SrtpProfile::Aes128CmHmacSha1_32),
            "AEAD_AES_128_GCM" => Ok(SrtpProfile::AeadAes128Gcm),
            "AEAD_AES_256_GCM" => Ok(SrtpProfile::AeadAes256Gcm),
            _ => Err(SrtpKeyError::InvalidCrypto(format!(
                "Unknown crypto suite: {}",
                suite
            ))),
        }
    }

    /// Parse crypto attribute and add to manager
    ///
    /// Convenience method that combines parsing and adding
    pub fn add_from_crypto_attr(
        &self,
        stream_id: String,
        ssrc: u32,
        crypto_line: &str,
    ) -> Result<()> {
        let (tag, suite, master_key, master_salt) = Self::parse_crypto_attribute(crypto_line)?;
        let profile = Self::suite_to_profile(&suite)?;

        let key_material = SrtpKeyMaterial::new(master_key, master_salt, profile)?;

        self.add_key(stream_id, ssrc, key_material, tag, suite);
        Ok(())
    }
}

impl Default for SrtpKeyManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper to extract crypto attributes from SDP
pub fn extract_crypto_from_sdp(sdp: &str) -> Vec<String> {
    sdp.lines()
        .filter(|line| line.starts_with("a=crypto:"))
        .map(|line| line.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_crypto_attribute() {
        let crypto =
            "a=crypto:1 AES_CM_128_HMAC_SHA1_80 inline:WVNfX19zZW1jdGwgKCkgewkyMjA7fQp9CnVubGVz";

        let result = SrtpKeyManager::parse_crypto_attribute(crypto);
        assert!(result.is_ok());

        let (tag, suite, master_key, master_salt) = result.unwrap();
        assert_eq!(tag, 1);
        assert_eq!(suite, "AES_CM_128_HMAC_SHA1_80");
        assert_eq!(master_key.len(), 16);
        assert_eq!(master_salt.len(), 14);
    }

    #[test]
    fn test_generate_crypto_attribute() {
        let master_key = vec![1u8; 16];
        let master_salt = vec![2u8; 14];

        let crypto = SrtpKeyManager::generate_crypto_attribute(
            1,
            "AES_CM_128_HMAC_SHA1_80",
            &master_key,
            &master_salt,
        );

        assert!(crypto.contains("a=crypto:1"));
        assert!(crypto.contains("AES_CM_128_HMAC_SHA1_80"));
        assert!(crypto.contains("inline:"));
    }

    #[test]
    fn test_suite_to_profile() {
        let profile = SrtpKeyManager::suite_to_profile("AES_CM_128_HMAC_SHA1_80").unwrap();
        assert_eq!(profile, SrtpProfile::Aes128CmHmacSha1_80);

        let profile = SrtpKeyManager::suite_to_profile("AEAD_AES_128_GCM").unwrap();
        assert_eq!(profile, SrtpProfile::AeadAes128Gcm);
    }

    #[test]
    fn test_key_manager_add_get() {
        let manager = SrtpKeyManager::new();
        let key_material = SrtpKeyMaterial::new(
            vec![0u8; 16],
            vec![0u8; 14],
            SrtpProfile::Aes128CmHmacSha1_80,
        )
        .unwrap();

        manager.add_key(
            "stream-1".to_string(),
            12345,
            key_material,
            1,
            "AES_CM_128_HMAC_SHA1_80".to_string(),
        );

        let info = manager.get_key_by_stream("stream-1").unwrap();
        assert_eq!(info.stream_id, "stream-1");
        assert_eq!(info.ssrc, 12345);

        let info_by_ssrc = manager.get_key_by_ssrc(12345).unwrap();
        assert_eq!(info_by_ssrc.stream_id, "stream-1");
    }

    #[test]
    fn test_extract_crypto_from_sdp() {
        let sdp = "v=0\r\n\
                   o=- 123 1 IN IP4 192.168.1.100\r\n\
                   s=Test\r\n\
                   a=crypto:1 AES_CM_128_HMAC_SHA1_80 inline:WVNfX19zZW1jdGwgKCkgewkyMjA7fQp9CnVubGVz\r\n\
                   a=rtpmap:0 PCMU/8000\r\n";

        let crypto_lines = extract_crypto_from_sdp(sdp);
        assert_eq!(crypto_lines.len(), 1);
        assert!(crypto_lines[0].contains("AES_CM_128_HMAC_SHA1_80"));
    }

    #[test]
    fn test_key_manager_remove() {
        let manager = SrtpKeyManager::new();
        let key_material = SrtpKeyMaterial::new(
            vec![0u8; 16],
            vec![0u8; 14],
            SrtpProfile::Aes128CmHmacSha1_80,
        )
        .unwrap();

        manager.add_key(
            "stream-1".to_string(),
            12345,
            key_material,
            1,
            "AES_CM_128_HMAC_SHA1_80".to_string(),
        );

        assert!(manager.get_key_by_stream("stream-1").is_some());

        manager.remove_key("stream-1");

        assert!(manager.get_key_by_stream("stream-1").is_none());
        assert!(manager.get_key_by_ssrc(12345).is_none());
    }
}
