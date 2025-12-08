//! SRTP encryption/decryption (RFC 3711)
//!
//! This module provides scaffolding for SRTP (Secure Real-time Transport Protocol)
//! support. Full cryptographic implementation is pending.

use forge_core::{ForgeError, Result};

/// SRTP profile as defined in RFC 5764 (DTLS-SRTP)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum SrtpProfile {
    /// SRTP_AES128_CM_HMAC_SHA1_80 (RFC 5764 Section 4.1.2)
    Aes128CmHmacSha1_80 = 0x0001,
    /// SRTP_AES128_CM_HMAC_SHA1_32 (RFC 5764 Section 4.1.2)
    Aes128CmHmacSha1_32 = 0x0002,
    /// SRTP_AEAD_AES_128_GCM (RFC 7714)
    AeadAes128Gcm = 0x0007,
    /// SRTP_AEAD_AES_256_GCM (RFC 7714)
    AeadAes256Gcm = 0x0008,
}

impl SrtpProfile {
    /// Get the master key length for this profile
    pub fn master_key_len(&self) -> usize {
        match self {
            SrtpProfile::Aes128CmHmacSha1_80 => 16,
            SrtpProfile::Aes128CmHmacSha1_32 => 16,
            SrtpProfile::AeadAes128Gcm => 16,
            SrtpProfile::AeadAes256Gcm => 32,
        }
    }

    /// Get the master salt length for this profile
    pub fn master_salt_len(&self) -> usize {
        match self {
            SrtpProfile::Aes128CmHmacSha1_80 => 14,
            SrtpProfile::Aes128CmHmacSha1_32 => 14,
            SrtpProfile::AeadAes128Gcm => 12,
            SrtpProfile::AeadAes256Gcm => 12,
        }
    }

    /// Get the authentication tag length
    pub fn auth_tag_len(&self) -> usize {
        match self {
            SrtpProfile::Aes128CmHmacSha1_80 => 10, // 80 bits
            SrtpProfile::Aes128CmHmacSha1_32 => 4,  // 32 bits
            SrtpProfile::AeadAes128Gcm => 16,       // 128 bits
            SrtpProfile::AeadAes256Gcm => 16,       // 128 bits
        }
    }
}

impl TryFrom<u16> for SrtpProfile {
    type Error = ForgeError;

    fn try_from(value: u16) -> Result<Self> {
        match value {
            0x0001 => Ok(SrtpProfile::Aes128CmHmacSha1_80),
            0x0002 => Ok(SrtpProfile::Aes128CmHmacSha1_32),
            0x0007 => Ok(SrtpProfile::AeadAes128Gcm),
            0x0008 => Ok(SrtpProfile::AeadAes256Gcm),
            _ => Err(ForgeError::Srtp(format!("Unknown SRTP profile: {:#04x}", value))),
        }
    }
}

/// SRTP key material
#[derive(Debug, Clone)]
pub struct SrtpKeyMaterial {
    /// Master key
    pub master_key: Vec<u8>,
    /// Master salt
    pub master_salt: Vec<u8>,
    /// SRTP profile
    pub profile: SrtpProfile,
}

impl SrtpKeyMaterial {
    /// Create new key material
    pub fn new(master_key: Vec<u8>, master_salt: Vec<u8>, profile: SrtpProfile) -> Result<Self> {
        // Validate key lengths
        if master_key.len() != profile.master_key_len() {
            return Err(ForgeError::Srtp(format!(
                "Invalid master key length: expected {}, got {}",
                profile.master_key_len(),
                master_key.len()
            )));
        }
        if master_salt.len() != profile.master_salt_len() {
            return Err(ForgeError::Srtp(format!(
                "Invalid master salt length: expected {}, got {}",
                profile.master_salt_len(),
                master_salt.len()
            )));
        }

        Ok(Self {
            master_key,
            master_salt,
            profile,
        })
    }
}

/// SRTP/SRTCP context for encryption and decryption
///
/// This is a placeholder structure for future SRTP implementation.
/// Full implementation requires:
/// - AES-CTR mode encryption
/// - HMAC-SHA1 or AES-GCM for authentication
/// - Key derivation (RFC 3711 Section 4.3)
/// - Replay protection with ROC (Rollover Counter)
pub struct SrtpContext {
    /// Key material for outbound (encrypting) traffic
    local_key: Option<SrtpKeyMaterial>,
    /// Key material for inbound (decrypting) traffic
    remote_key: Option<SrtpKeyMaterial>,
    /// Replay protection window
    #[allow(dead_code)]
    replay_window: ReplayWindow,
}

impl SrtpContext {
    /// Create a new SRTP context without keys (passthrough mode)
    pub fn new() -> Self {
        Self {
            local_key: None,
            remote_key: None,
            replay_window: ReplayWindow::new(64),
        }
    }

    /// Create a new SRTP context with keys
    pub fn with_keys(local_key: SrtpKeyMaterial, remote_key: SrtpKeyMaterial) -> Self {
        Self {
            local_key: Some(local_key),
            remote_key: Some(remote_key),
            replay_window: ReplayWindow::new(64),
        }
    }

    /// Set local (outbound) key material
    pub fn set_local_key(&mut self, key: SrtpKeyMaterial) {
        self.local_key = Some(key);
    }

    /// Set remote (inbound) key material
    pub fn set_remote_key(&mut self, key: SrtpKeyMaterial) {
        self.remote_key = Some(key);
    }

    /// Encrypt an RTP packet to SRTP
    ///
    /// TODO: Implement actual SRTP encryption
    /// For now, this is a passthrough that returns an error if keys are set
    pub fn protect_rtp(&mut self, packet: &[u8]) -> Result<Vec<u8>> {
        if self.local_key.is_some() {
            // TODO: Implement SRTP encryption
            return Err(ForgeError::Srtp(
                "SRTP encryption not yet implemented".to_string(),
            ));
        }
        // Passthrough mode
        Ok(packet.to_vec())
    }

    /// Decrypt an SRTP packet to RTP
    ///
    /// TODO: Implement actual SRTP decryption
    /// For now, this is a passthrough that returns an error if keys are set
    pub fn unprotect_rtp(&mut self, packet: &[u8]) -> Result<Vec<u8>> {
        if self.remote_key.is_some() {
            // TODO: Implement SRTP decryption
            return Err(ForgeError::Srtp(
                "SRTP decryption not yet implemented".to_string(),
            ));
        }
        // Passthrough mode
        Ok(packet.to_vec())
    }

    /// Encrypt an RTCP packet to SRTCP
    ///
    /// TODO: Implement actual SRTCP encryption
    pub fn protect_rtcp(&mut self, packet: &[u8]) -> Result<Vec<u8>> {
        if self.local_key.is_some() {
            return Err(ForgeError::Srtp(
                "SRTCP encryption not yet implemented".to_string(),
            ));
        }
        Ok(packet.to_vec())
    }

    /// Decrypt an SRTCP packet to RTCP
    ///
    /// TODO: Implement actual SRTCP decryption
    pub fn unprotect_rtcp(&mut self, packet: &[u8]) -> Result<Vec<u8>> {
        if self.remote_key.is_some() {
            return Err(ForgeError::Srtp(
                "SRTCP decryption not yet implemented".to_string(),
            ));
        }
        Ok(packet.to_vec())
    }

    /// Check if SRTP is enabled (keys are configured)
    pub fn is_enabled(&self) -> bool {
        self.local_key.is_some() || self.remote_key.is_some()
    }
}

impl Default for SrtpContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Replay protection window for SRTP
///
/// This tracks received packet indices to prevent replay attacks.
/// Uses a sliding window bitmap for efficient lookups.
struct ReplayWindow {
    /// Window size
    window_size: usize,
    /// Highest received sequence number
    highest_seq: u64,
    /// Bitmap of received packets (bit N represents packet highest_seq - N)
    bitmap: u64,
}

impl ReplayWindow {
    /// Create a new replay window
    fn new(window_size: usize) -> Self {
        Self {
            window_size: window_size.min(64), // Limited by bitmap size
            highest_seq: 0,
            bitmap: 0,
        }
    }

    /// Check if a sequence number has been seen (replay attack)
    #[allow(dead_code)]
    fn check(&self, seq: u64) -> bool {
        if seq > self.highest_seq {
            // New packet
            return false;
        }

        let delta = self.highest_seq - seq;
        if delta >= self.window_size as u64 {
            // Too old
            return true;
        }

        // Check bitmap
        (self.bitmap & (1u64 << delta)) != 0
    }

    /// Update the window with a new sequence number
    #[allow(dead_code)]
    fn update(&mut self, seq: u64) {
        if seq > self.highest_seq {
            // Shift window
            let delta = seq - self.highest_seq;
            if delta < 64 {
                self.bitmap <<= delta;
                self.bitmap |= 1;
            } else {
                self.bitmap = 1;
            }
            self.highest_seq = seq;
        } else {
            // Mark in bitmap
            let delta = self.highest_seq - seq;
            if delta < 64 {
                self.bitmap |= 1u64 << delta;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_srtp_profile_lengths() {
        let profile = SrtpProfile::Aes128CmHmacSha1_80;
        assert_eq!(profile.master_key_len(), 16);
        assert_eq!(profile.master_salt_len(), 14);
        assert_eq!(profile.auth_tag_len(), 10);

        let profile = SrtpProfile::AeadAes128Gcm;
        assert_eq!(profile.master_key_len(), 16);
        assert_eq!(profile.master_salt_len(), 12);
        assert_eq!(profile.auth_tag_len(), 16);
    }

    #[test]
    fn test_key_material_validation() {
        let profile = SrtpProfile::Aes128CmHmacSha1_80;

        // Valid keys
        let key = vec![0u8; 16];
        let salt = vec![0u8; 14];
        assert!(SrtpKeyMaterial::new(key.clone(), salt.clone(), profile).is_ok());

        // Invalid key length
        let bad_key = vec![0u8; 15];
        assert!(SrtpKeyMaterial::new(bad_key, salt.clone(), profile).is_err());

        // Invalid salt length
        let bad_salt = vec![0u8; 13];
        assert!(SrtpKeyMaterial::new(key, bad_salt, profile).is_err());
    }

    #[test]
    fn test_srtp_context_passthrough() {
        let mut ctx = SrtpContext::new();
        assert!(!ctx.is_enabled());

        let packet = vec![1, 2, 3, 4, 5];

        // Passthrough mode should work
        let protected = ctx.protect_rtp(&packet).unwrap();
        assert_eq!(protected, packet);

        let unprotected = ctx.unprotect_rtp(&packet).unwrap();
        assert_eq!(unprotected, packet);
    }

    #[test]
    fn test_replay_window() {
        let mut window = ReplayWindow::new(64);

        // First packet
        assert!(!window.check(100));
        window.update(100);
        assert!(window.check(100));

        // Newer packet
        assert!(!window.check(105));
        window.update(105);
        assert!(window.check(105));

        // Old packet within window
        assert!(!window.check(102));
        window.update(102);
        assert!(window.check(102));

        // Too old packet
        assert!(window.check(40));
    }
}
