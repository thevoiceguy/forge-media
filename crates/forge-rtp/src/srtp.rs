//! SRTP encryption/decryption (RFC 3711)
//!
//! This module provides SRTP (Secure Real-time Transport Protocol) implementation
//! with support for AES-CM and AES-GCM cipher suites.

use aes::cipher::{BlockEncrypt, KeyInit};
use aes::{Aes128, Aes256};
use aes_gcm::{AeadInPlace, Aes128Gcm, Aes256Gcm, Nonce};
use forge_core::{ForgeError, Result};
use hmac::{Hmac, Mac};
use metrics::counter;
use sha1::Sha1;
use subtle::ConstantTimeEq;

type HmacSha1 = Hmac<Sha1>;

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
            _ => Err(ForgeError::Srtp(format!(
                "Unknown SRTP profile: {:#04x}",
                value
            ))),
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

    /// Derive SRTP session keys (RFC 3711 §4.3.3 labels 0x00/0x01/0x02).
    ///
    /// Use this for `protect_rtp` / `unprotect_rtp`. For SRTCP, call
    /// [`Self::derive_srtcp_session_keys`] which uses the SRTCP-specific
    /// labels (0x03/0x04/0x05). Using SRTP labels for SRTCP derives a
    /// completely different (and wrong) auth key, so the peer's
    /// auth-tag check fails on every SRTCP packet — that exact
    /// production symptom is what motivated splitting the two.
    fn derive_srtp_session_keys(&self, ssrc: u32, index: u64) -> Result<DerivedKeys> {
        self.derive_session_keys_with_labels(
            ssrc, index, /* enc */ 0x00, /* auth */ 0x01, /* salt */ 0x02,
        )
    }

    /// Derive SRTCP session keys (RFC 3711 §4.3.3 labels 0x03/0x04/0x05).
    ///
    /// SRTCP shares the master_key + master_salt with SRTP but derives
    /// distinct session keys via different KDF labels (per RFC 3711
    /// §4.3.3 "List of Reserved Labels"). The session-encryption key
    /// is unrelated to the SRTP one — using SRTP's would also encrypt,
    /// but the auth tag the peer computes (correctly with SRTCP
    /// labels) would never match ours.
    fn derive_srtcp_session_keys(&self, ssrc: u32, index: u64) -> Result<DerivedKeys> {
        self.derive_session_keys_with_labels(
            ssrc, index, /* enc */ 0x03, /* auth */ 0x04, /* salt */ 0x05,
        )
    }

    /// Common implementation for both SRTP and SRTCP — the two only
    /// differ in which set of KDF labels they pass in.
    fn derive_session_keys_with_labels(
        &self,
        ssrc: u32,
        index: u64,
        enc_label: u8,
        auth_label: u8,
        salt_label: u8,
    ) -> Result<DerivedKeys> {
        match self.profile {
            SrtpProfile::Aes128CmHmacSha1_80 | SrtpProfile::Aes128CmHmacSha1_32 => {
                // AES-CM with HMAC-SHA1
                let enc_key = self.derive_key(enc_label, ssrc, index, 16)?; // 128-bit encryption key
                let auth_key = self.derive_key(auth_label, ssrc, index, 20)?; // 160-bit auth key
                let salt = self.derive_key(salt_label, ssrc, index, 14)?; // 112-bit salt

                Ok(DerivedKeys {
                    encryption_key: enc_key,
                    authentication_key: Some(auth_key),
                    salt,
                })
            }
            SrtpProfile::AeadAes128Gcm | SrtpProfile::AeadAes256Gcm => {
                // AES-GCM (AEAD - no separate auth key)
                let key_len = self.profile.master_key_len();
                let enc_key = self.derive_key(enc_label, ssrc, index, key_len)?;
                let salt = self.derive_key(salt_label, ssrc, index, 12)?; // 96-bit salt for GCM

                Ok(DerivedKeys {
                    encryption_key: enc_key,
                    authentication_key: None, // AEAD mode doesn't use separate auth key
                    salt,
                })
            }
        }
    }

    /// Key derivation function from RFC 3711 Section 4.3
    ///
    /// Derives a key of specified length using AES-CM as PRF
    ///
    /// # Arguments
    /// * `label` - Key derivation label (0x00=encryption, 0x01=auth, 0x02=salt)
    /// * `ssrc` - Synchronization source identifier (not used in basic derivation)
    /// * `index` - Packet index for key derivation rate
    /// * `out_len` - Desired output key length in bytes
    fn derive_key(&self, label: u8, _ssrc: u32, index: u64, out_len: usize) -> Result<Vec<u8>> {
        // DIV = index DIV key_derivation_rate
        // For r=0 (default), DIV = 0
        let r = 0u64; // key_derivation_rate = 2^r
        let div = if r == 0 { 0u64 } else { index >> r };

        // RFC 3711 Section 4.3.3: key_id = <label> || r
        // x = key_id XOR master_salt (padded to 16 bytes with zeros)
        let mut x = [0u8; 16];

        // Set label in byte 7 (RFC 3711 uses specific byte positions)
        x[7] = label;

        // Set r (DIV) in bytes 8-13 (48 bits, big-endian)
        let div_bytes = div.to_be_bytes();
        x[8..14].copy_from_slice(&div_bytes[2..8]);

        // XOR with master_salt (master_salt is typically 14 bytes)
        for (i, &salt_byte) in self.master_salt.iter().enumerate() {
            x[i] ^= salt_byte;
        }

        // Derive key using AES-CM (Counter Mode)
        self.aes_cm_prf(&x, out_len)
    }

    /// AES Counter Mode PRF (Pseudo-Random Function)
    ///
    /// Generates `out_len` bytes of output using AES in counter mode
    /// Automatically selects AES-128 or AES-256 based on master key length
    fn aes_cm_prf(&self, iv: &[u8], out_len: usize) -> Result<Vec<u8>> {
        let mut output = vec![0u8; out_len];
        let mut counter_block = [0u8; 16];

        // Copy IV to counter block (pad with zeros if needed)
        let copy_len = iv.len().min(16);
        counter_block[..copy_len].copy_from_slice(&iv[..copy_len]);

        // Generate output blocks
        let num_blocks = out_len.div_ceil(16); // Round up to block count

        // Use AES-128 or AES-256 based on master key length
        match self.master_key.len() {
            16 => {
                // AES-128
                let cipher = Aes128::new_from_slice(&self.master_key).map_err(|e| {
                    ForgeError::Srtp(format!("Failed to create AES-128 cipher: {}", e))
                })?;

                for i in 0..num_blocks {
                    // Encrypt counter block
                    let mut block = aes::Block::from(counter_block);
                    cipher.encrypt_block(&mut block);

                    // Copy to output (handle last partial block)
                    let offset = i * 16;
                    let copy_len = (out_len - offset).min(16);
                    output[offset..offset + copy_len].copy_from_slice(&block[..copy_len]);

                    // Increment counter (treat as big-endian 128-bit integer)
                    for j in (0..16).rev() {
                        counter_block[j] = counter_block[j].wrapping_add(1);
                        if counter_block[j] != 0 {
                            break; // No carry needed
                        }
                    }
                }
            }
            32 => {
                // AES-256
                let cipher = Aes256::new_from_slice(&self.master_key).map_err(|e| {
                    ForgeError::Srtp(format!("Failed to create AES-256 cipher: {}", e))
                })?;

                for i in 0..num_blocks {
                    // Encrypt counter block
                    let mut block = aes::Block::from(counter_block);
                    cipher.encrypt_block(&mut block);

                    // Copy to output (handle last partial block)
                    let offset = i * 16;
                    let copy_len = (out_len - offset).min(16);
                    output[offset..offset + copy_len].copy_from_slice(&block[..copy_len]);

                    // Increment counter (treat as big-endian 128-bit integer)
                    for j in (0..16).rev() {
                        counter_block[j] = counter_block[j].wrapping_add(1);
                        if counter_block[j] != 0 {
                            break; // No carry needed
                        }
                    }
                }
            }
            len => {
                return Err(ForgeError::Srtp(format!(
                    "Invalid master key length: expected 16 or 32 bytes, got {}",
                    len
                )));
            }
        }

        Ok(output)
    }
}

/// Derived session keys for SRTP/SRTCP
#[derive(Debug, Clone)]
struct DerivedKeys {
    /// Session encryption key
    encryption_key: Vec<u8>,
    /// Session authentication key (None for AEAD modes)
    authentication_key: Option<Vec<u8>>,
    /// Session salt
    salt: Vec<u8>,
}

/// ROC (Rollover Counter) tracker per SSRC
///
/// Tracks the rollover counter for sequence number wrapping (65535 → 0)
#[derive(Debug, Clone)]
struct RocTracker {
    /// Rollover counter - increments when sequence number wraps
    roc: u32,
    /// Highest sequence number seen
    highest_seq: u16,
    /// Local sequence from last resync (s_l in RFC 3711)
    s_l: u16,
}

impl RocTracker {
    fn new() -> Self {
        Self {
            roc: 0,
            highest_seq: 0,
            s_l: 0,
        }
    }

    /// Get the extended sequence number (48-bit: 32-bit ROC + 16-bit seq)
    fn get_index(&self, seq: u16) -> u64 {
        ((self.roc as u64) << 16) | (seq as u64)
    }

    /// Update ROC based on received sequence number
    ///
    /// Implements RFC 3711 Section 3.3.1 index determination
    fn update(&mut self, seq: u16) {
        if self.highest_seq == 0 && self.roc == 0 {
            // First packet
            self.highest_seq = seq;
            self.s_l = seq;
            return;
        }

        // Calculate v (estimated ROC)
        let delta = seq as i32 - self.s_l as i32;
        let v = if delta > 32768 {
            // Wrapped backwards
            self.roc.wrapping_sub(1)
        } else if delta < -32768 {
            // Wrapped forwards
            self.roc.wrapping_add(1)
        } else {
            self.roc
        };

        // Update highest_seq if this packet is newer
        let current_index = ((self.roc as u64) << 16) | (self.highest_seq as u64);
        let new_index = ((v as u64) << 16) | (seq as u64);

        if new_index > current_index {
            self.highest_seq = seq;
            self.roc = v;
            self.s_l = seq;
        }
    }

    /// Get ROC for a given sequence number (for decryption)
    fn get_roc(&self, seq: u16) -> u32 {
        // Handle first packet
        if self.highest_seq == 0 && self.roc == 0 {
            return 0;
        }

        let delta = seq as i32 - self.s_l as i32;
        if delta > 32768 {
            self.roc.wrapping_sub(1)
        } else if delta < -32768 {
            self.roc.wrapping_add(1)
        } else {
            self.roc
        }
    }
}

/// SRTP/SRTCP context for encryption and decryption
///
/// Implements RFC 3711 SRTP encryption, authentication, and replay protection
pub struct SrtpContext {
    /// Key material for outbound (encrypting) traffic
    local_key: Option<SrtpKeyMaterial>,
    /// Key material for inbound (decrypting) traffic
    remote_key: Option<SrtpKeyMaterial>,
    /// Replay protection window for RTP
    replay_window: ReplayWindow,
    /// Replay protection window for RTCP (separate per RFC 3711 Section 3.4)
    srtcp_replay_window: ReplayWindow,
    /// ROC tracker for local (outbound) SSRC
    local_roc: RocTracker,
    /// ROC tracker for remote (inbound) SSRC
    remote_roc: RocTracker,
    /// SRTCP index for local (outbound) RTCP packets
    local_srtcp_index: u32,
    /// SRTCP index for remote (inbound) RTCP packets (highest received)
    remote_srtcp_index: u32,
}

impl SrtpContext {
    /// Create a new SRTP context without keys (passthrough mode)
    pub fn new() -> Self {
        Self {
            local_key: None,
            remote_key: None,
            replay_window: ReplayWindow::new(64),
            srtcp_replay_window: ReplayWindow::new(64),
            local_roc: RocTracker::new(),
            remote_roc: RocTracker::new(),
            local_srtcp_index: 0,
            remote_srtcp_index: 0,
        }
    }

    /// Create a new SRTP context with keys
    pub fn with_keys(local_key: SrtpKeyMaterial, remote_key: SrtpKeyMaterial) -> Self {
        Self {
            local_key: Some(local_key),
            remote_key: Some(remote_key),
            replay_window: ReplayWindow::new(64),
            srtcp_replay_window: ReplayWindow::new(64),
            local_roc: RocTracker::new(),
            remote_roc: RocTracker::new(),
            local_srtcp_index: 0,
            remote_srtcp_index: 0,
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
    /// Implements RFC 3711 Section 3.3 SRTP packet processing
    pub fn protect_rtp(&mut self, packet: &[u8]) -> Result<Vec<u8>> {
        // Passthrough mode if no keys configured
        let Some(ref key_material) = self.local_key else {
            return Ok(packet.to_vec());
        };

        // Parse RTP header (minimum 12 bytes)
        if packet.len() < 12 {
            return Err(ForgeError::Srtp("RTP packet too short".to_string()));
        }

        // Extract RTP header fields
        let _version = (packet[0] >> 6) & 0x03;
        let _padding = (packet[0] >> 5) & 0x01;
        let extension = (packet[0] >> 4) & 0x01;
        let csrc_count = packet[0] & 0x0F;
        let _payload_type = packet[1] & 0x7F;
        let sequence = u16::from_be_bytes([packet[2], packet[3]]);
        let _timestamp = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);
        let ssrc = u32::from_be_bytes([packet[8], packet[9], packet[10], packet[11]]);

        // Calculate header length (fixed 12 bytes + CSRCs + extension).
        // All arithmetic is checked to avoid integer overflow from attacker-controlled
        // CC (0..=15) and extension length (0..=u16::MAX) fields.
        let mut header_len = 12usize
            .checked_add((csrc_count as usize).saturating_mul(4))
            .ok_or_else(|| ForgeError::Srtp("RTP header length overflow".to_string()))?;

        if extension == 1 {
            let after_csrc = header_len
                .checked_add(4)
                .ok_or_else(|| ForgeError::Srtp("RTP extension length overflow".to_string()))?;
            if packet.len() < after_csrc {
                return Err(ForgeError::Srtp("Invalid RTP header extension".to_string()));
            }
            let ext_len_words =
                u16::from_be_bytes([packet[header_len + 2], packet[header_len + 3]]) as usize;
            let ext_bytes = ext_len_words
                .checked_mul(4)
                .ok_or_else(|| ForgeError::Srtp("RTP extension length overflow".to_string()))?;
            header_len = after_csrc
                .checked_add(ext_bytes)
                .ok_or_else(|| ForgeError::Srtp("RTP extension length overflow".to_string()))?;
        }

        if packet.len() < header_len {
            return Err(ForgeError::Srtp("Invalid RTP header".to_string()));
        }

        // Update ROC
        self.local_roc.update(sequence);
        let roc = self.local_roc.roc;
        let packet_index = self.local_roc.get_index(sequence);

        // Derive session keys
        let derived_keys = key_material.derive_srtp_session_keys(ssrc, packet_index)?;

        // Encrypt payload based on profile
        let srtp_packet = match key_material.profile {
            SrtpProfile::Aes128CmHmacSha1_80 | SrtpProfile::Aes128CmHmacSha1_32 => {
                // AES-CM encryption + HMAC-SHA1 authentication
                self.protect_aes_cm(
                    packet,
                    header_len,
                    &derived_keys,
                    ssrc,
                    roc,
                    packet_index,
                    key_material.profile,
                )?
            }
            SrtpProfile::AeadAes128Gcm | SrtpProfile::AeadAes256Gcm => {
                // AES-GCM AEAD
                self.protect_aes_gcm(
                    packet,
                    header_len,
                    &derived_keys,
                    ssrc,
                    roc,
                    sequence,
                    key_material.profile,
                )?
            }
        };

        // Increment metrics counter
        counter!("forge_srtp_packets_encrypted_total", 1);

        Ok(srtp_packet)
    }

    /// Protect RTP packet using AES-CM + HMAC-SHA1
    fn protect_aes_cm(
        &self,
        packet: &[u8],
        header_len: usize,
        keys: &DerivedKeys,
        ssrc: u32,
        roc: u32,
        packet_index: u64,
        profile: SrtpProfile,
    ) -> Result<Vec<u8>> {
        // Construct IV for AES-CTR: salt XOR (SSRC || packet_index)
        let mut iv = [0u8; 16];
        iv[..keys.salt.len()].copy_from_slice(&keys.salt);

        // XOR with SSRC (bytes 4-7)
        let ssrc_bytes = ssrc.to_be_bytes();
        for i in 0..4 {
            iv[4 + i] ^= ssrc_bytes[i];
        }

        // XOR with packet index at bytes 8-13 (48 bits, big-endian MSB at 8).
        // Per RFC 3711 §4.1.1: IV = (k_s * 2^16) XOR (SSRC * 2^64) XOR (i * 2^16).
        // i << 16 occupies bits 16..63 of the 128-bit IV, i.e. bytes 8..13
        // (MSB at byte 8, LSB at byte 13) in big-endian byte order.
        let index_bytes = packet_index.to_be_bytes();
        for i in 0..6 {
            iv[8 + i] ^= index_bytes[2 + i];
        }

        // Encrypt payload using AES-CTR
        let cipher = Aes128::new_from_slice(&keys.encryption_key)
            .map_err(|e| ForgeError::Srtp(format!("Failed to create AES cipher: {}", e)))?;

        let mut encrypted = packet.to_vec();
        let payload = &mut encrypted[header_len..];

        // AES-CTR: encrypt counter blocks and XOR with payload
        let mut counter_block = iv;
        let mut offset = 0;

        while offset < payload.len() {
            // Encrypt counter block
            let mut block = aes::Block::from(counter_block);
            cipher.encrypt_block(&mut block);

            // XOR with payload
            let copy_len = (payload.len() - offset).min(16);
            for i in 0..copy_len {
                payload[offset + i] ^= block[i];
            }

            offset += 16;

            // Increment counter
            for j in (0..16).rev() {
                counter_block[j] = counter_block[j].wrapping_add(1);
                if counter_block[j] != 0 {
                    break;
                }
            }
        }

        // Compute HMAC-SHA1 over: RTP header + encrypted payload + ROC
        let auth_key = keys
            .authentication_key
            .as_ref()
            .ok_or_else(|| ForgeError::Srtp("Missing authentication key".to_string()))?;

        let mut mac = <HmacSha1 as Mac>::new_from_slice(auth_key)
            .map_err(|e| ForgeError::Srtp(format!("Failed to create HMAC: {}", e)))?;

        mac.update(&encrypted);
        mac.update(&roc.to_be_bytes());

        let auth_tag = mac.finalize().into_bytes();
        let tag_len = profile.auth_tag_len();

        // Append truncated auth tag
        encrypted.extend_from_slice(&auth_tag[..tag_len]);

        Ok(encrypted)
    }

    /// Protect RTP packet using AES-GCM AEAD
    fn protect_aes_gcm(
        &self,
        packet: &[u8],
        header_len: usize,
        keys: &DerivedKeys,
        ssrc: u32,
        roc: u32,
        sequence: u16,
        profile: SrtpProfile,
    ) -> Result<Vec<u8>> {
        // Construct nonce (96 bits): salt XOR (SSRC || ROC || SEQ)
        let mut nonce_bytes = [0u8; 12];
        nonce_bytes[..keys.salt.len()].copy_from_slice(&keys.salt);

        // XOR with SSRC (bytes 2-5)
        let ssrc_bytes = ssrc.to_be_bytes();
        for i in 0..4 {
            nonce_bytes[2 + i] ^= ssrc_bytes[i];
        }

        // XOR with ROC (bytes 6-9)
        let roc_bytes = roc.to_be_bytes();
        for i in 0..4 {
            nonce_bytes[6 + i] ^= roc_bytes[i];
        }

        // XOR with sequence (bytes 10-11)
        let seq_bytes = sequence.to_be_bytes();
        for i in 0..2 {
            nonce_bytes[10 + i] ^= seq_bytes[i];
        }

        let nonce = Nonce::from_slice(&nonce_bytes);

        // Split packet into header (AAD) and payload
        let mut payload = packet[header_len..].to_vec();

        // Encrypt and get tag separately (per RFC 7714)
        let tag = match profile {
            SrtpProfile::AeadAes128Gcm => {
                let cipher = Aes128Gcm::new_from_slice(&keys.encryption_key).map_err(|e| {
                    ForgeError::Srtp(format!("Failed to create AES-GCM cipher: {}", e))
                })?;

                cipher
                    .encrypt_in_place_detached(nonce, &packet[..header_len], &mut payload)
                    .map_err(|e| ForgeError::Srtp(format!("AES-GCM encryption failed: {}", e)))?
            }
            SrtpProfile::AeadAes256Gcm => {
                let cipher = Aes256Gcm::new_from_slice(&keys.encryption_key).map_err(|e| {
                    ForgeError::Srtp(format!("Failed to create AES-GCM cipher: {}", e))
                })?;

                cipher
                    .encrypt_in_place_detached(nonce, &packet[..header_len], &mut payload)
                    .map_err(|e| ForgeError::Srtp(format!("AES-GCM encryption failed: {}", e)))?
            }
            _ => unreachable!(),
        };

        // Build result: header + encrypted_payload + auth_tag
        let mut result = Vec::with_capacity(packet.len() + 16);
        result.extend_from_slice(&packet[..header_len]); // Header
        result.extend_from_slice(&payload); // Encrypted payload
        result.extend_from_slice(&tag); // Auth tag
        Ok(result)
    }

    /// Decrypt an SRTP packet to RTP
    ///
    /// Implements RFC 3711 Section 3.4 SRTP packet processing
    pub fn unprotect_rtp(&mut self, packet: &[u8]) -> Result<Vec<u8>> {
        // Passthrough mode if no keys configured
        let Some(ref key_material) = self.remote_key else {
            return Ok(packet.to_vec());
        };

        // Parse RTP header
        if packet.len() < 12 {
            return Err(ForgeError::Srtp("SRTP packet too short".to_string()));
        }

        let _version = (packet[0] >> 6) & 0x03;
        let extension = (packet[0] >> 4) & 0x01;
        let csrc_count = packet[0] & 0x0F;
        let sequence = u16::from_be_bytes([packet[2], packet[3]]);
        let ssrc = u32::from_be_bytes([packet[8], packet[9], packet[10], packet[11]]);

        // Calculate header length. All arithmetic is checked to prevent integer
        // overflow from attacker-controlled CC (0..=15) and extension length
        // (0..=u16::MAX) fields, which could otherwise bypass the subsequent
        // `packet.len() < header_len + auth_tag_len` bounds check and enable an
        // out-of-bounds read (see audit finding C1).
        let mut header_len = 12usize
            .checked_add((csrc_count as usize).saturating_mul(4))
            .ok_or_else(|| ForgeError::Srtp("RTP header length overflow".to_string()))?;

        if extension == 1 {
            let after_csrc = header_len
                .checked_add(4)
                .ok_or_else(|| ForgeError::Srtp("RTP extension length overflow".to_string()))?;
            if packet.len() < after_csrc {
                return Err(ForgeError::Srtp("Invalid RTP header extension".to_string()));
            }
            let ext_len_words =
                u16::from_be_bytes([packet[header_len + 2], packet[header_len + 3]]) as usize;
            let ext_bytes = ext_len_words
                .checked_mul(4)
                .ok_or_else(|| ForgeError::Srtp("RTP extension length overflow".to_string()))?;
            header_len = after_csrc
                .checked_add(ext_bytes)
                .ok_or_else(|| ForgeError::Srtp("RTP extension length overflow".to_string()))?;
        }

        let auth_tag_len = key_material.profile.auth_tag_len();

        let header_plus_tag = header_len
            .checked_add(auth_tag_len)
            .ok_or_else(|| ForgeError::Srtp("SRTP packet length overflow".to_string()))?;
        if packet.len() < header_plus_tag {
            return Err(ForgeError::Srtp(
                "SRTP packet too short for auth tag".to_string(),
            ));
        }

        // Determine ROC
        let roc = self.remote_roc.get_roc(sequence);
        let packet_index = ((roc as u64) << 16) | (sequence as u64);

        // Check replay protection
        if self.replay_window.check(packet_index) {
            counter!("forge_srtp_replay_attacks_blocked_total", 1);
            return Err(ForgeError::Srtp("Replay attack detected".to_string()));
        }

        // Derive session keys
        let derived_keys = key_material.derive_srtp_session_keys(ssrc, packet_index)?;

        // Decrypt based on profile
        let rtp_packet = match key_material.profile {
            SrtpProfile::Aes128CmHmacSha1_80 | SrtpProfile::Aes128CmHmacSha1_32 => self
                .unprotect_aes_cm(
                    packet,
                    header_len,
                    &derived_keys,
                    ssrc,
                    roc,
                    packet_index,
                    key_material.profile,
                )?,
            SrtpProfile::AeadAes128Gcm | SrtpProfile::AeadAes256Gcm => self.unprotect_aes_gcm(
                packet,
                header_len,
                &derived_keys,
                ssrc,
                roc,
                sequence,
                key_material.profile,
            )?,
        };

        // Update replay window
        self.replay_window.update(packet_index);
        self.remote_roc.update(sequence);

        // Increment metrics counter
        counter!("forge_srtp_packets_decrypted_total", 1);

        Ok(rtp_packet)
    }

    /// Unprotect SRTP packet using AES-CM + HMAC-SHA1
    fn unprotect_aes_cm(
        &self,
        packet: &[u8],
        header_len: usize,
        keys: &DerivedKeys,
        ssrc: u32,
        roc: u32,
        packet_index: u64,
        profile: SrtpProfile,
    ) -> Result<Vec<u8>> {
        let tag_len = profile.auth_tag_len();

        if packet.len() < header_len + tag_len {
            return Err(ForgeError::Srtp("Packet too short".to_string()));
        }

        let encrypted_len = packet.len() - tag_len;
        let encrypted_packet = &packet[..encrypted_len];
        let received_tag = &packet[encrypted_len..];

        // Verify HMAC-SHA1
        let auth_key = keys
            .authentication_key
            .as_ref()
            .ok_or_else(|| ForgeError::Srtp("Missing authentication key".to_string()))?;

        let mut mac = <HmacSha1 as Mac>::new_from_slice(auth_key)
            .map_err(|e| ForgeError::Srtp(format!("Failed to create HMAC: {}", e)))?;

        mac.update(encrypted_packet);
        mac.update(&roc.to_be_bytes());

        let computed_tag = mac.finalize().into_bytes();

        // Constant-time comparison
        if !bool::from(computed_tag[..tag_len].ct_eq(received_tag)) {
            return Err(ForgeError::Srtp("Authentication failed".to_string()));
        }

        // Construct IV for AES-CTR
        let mut iv = [0u8; 16];
        iv[..keys.salt.len()].copy_from_slice(&keys.salt);

        let ssrc_bytes = ssrc.to_be_bytes();
        for i in 0..4 {
            iv[4 + i] ^= ssrc_bytes[i];
        }

        // Packet index at bytes 8-13 (RFC 3711 §4.1.1: i << 16 → bits 16..63).
        let index_bytes = packet_index.to_be_bytes();
        for i in 0..6 {
            iv[8 + i] ^= index_bytes[2 + i];
        }

        // Decrypt payload
        let cipher = Aes128::new_from_slice(&keys.encryption_key)
            .map_err(|e| ForgeError::Srtp(format!("Failed to create AES cipher: {}", e)))?;

        let mut decrypted = encrypted_packet.to_vec();
        let payload = &mut decrypted[header_len..];

        let mut counter_block = iv;
        let mut offset = 0;

        while offset < payload.len() {
            let mut block = aes::Block::from(counter_block);
            cipher.encrypt_block(&mut block);

            let copy_len = (payload.len() - offset).min(16);
            for i in 0..copy_len {
                payload[offset + i] ^= block[i];
            }

            offset += 16;

            for j in (0..16).rev() {
                counter_block[j] = counter_block[j].wrapping_add(1);
                if counter_block[j] != 0 {
                    break;
                }
            }
        }

        Ok(decrypted)
    }

    /// Unprotect SRTP packet using AES-GCM AEAD
    fn unprotect_aes_gcm(
        &self,
        packet: &[u8],
        header_len: usize,
        keys: &DerivedKeys,
        ssrc: u32,
        roc: u32,
        sequence: u16,
        profile: SrtpProfile,
    ) -> Result<Vec<u8>> {
        // Construct nonce
        let mut nonce_bytes = [0u8; 12];
        nonce_bytes[..keys.salt.len()].copy_from_slice(&keys.salt);

        let ssrc_bytes = ssrc.to_be_bytes();
        for i in 0..4 {
            nonce_bytes[2 + i] ^= ssrc_bytes[i];
        }

        let roc_bytes = roc.to_be_bytes();
        for i in 0..4 {
            nonce_bytes[6 + i] ^= roc_bytes[i];
        }

        let seq_bytes = sequence.to_be_bytes();
        for i in 0..2 {
            nonce_bytes[10 + i] ^= seq_bytes[i];
        }

        let nonce = Nonce::from_slice(&nonce_bytes);

        // Extract auth tag (last 16 bytes)
        let tag_len = 16;
        if packet.len() < header_len + tag_len {
            return Err(ForgeError::Srtp(
                "SRTP packet too short for AES-GCM".to_string(),
            ));
        }

        let tag_start = packet.len() - tag_len;
        let tag = &packet[tag_start..];

        // Extract ciphertext (between header and tag)
        let mut ciphertext = packet[header_len..tag_start].to_vec();

        // Decrypt and verify with detached tag
        use aes_gcm::Tag;

        let tag_array: &[u8; 16] = tag
            .try_into()
            .map_err(|_| ForgeError::Srtp("Invalid tag length".to_string()))?;
        let tag_obj = Tag::from_slice(tag_array);

        match profile {
            SrtpProfile::AeadAes128Gcm => {
                let cipher = Aes128Gcm::new_from_slice(&keys.encryption_key).map_err(|e| {
                    ForgeError::Srtp(format!("Failed to create AES-GCM cipher: {}", e))
                })?;

                cipher
                    .decrypt_in_place_detached(
                        nonce,
                        &packet[..header_len],
                        &mut ciphertext,
                        tag_obj,
                    )
                    .map_err(|e| {
                        ForgeError::Srtp(format!("AES-GCM decryption/verification failed: {}", e))
                    })?;
            }
            SrtpProfile::AeadAes256Gcm => {
                let cipher = Aes256Gcm::new_from_slice(&keys.encryption_key).map_err(|e| {
                    ForgeError::Srtp(format!("Failed to create AES-GCM cipher: {}", e))
                })?;

                cipher
                    .decrypt_in_place_detached(
                        nonce,
                        &packet[..header_len],
                        &mut ciphertext,
                        tag_obj,
                    )
                    .map_err(|e| {
                        ForgeError::Srtp(format!("AES-GCM decryption/verification failed: {}", e))
                    })?;
            }
            _ => unreachable!(),
        }

        // Return header + decrypted payload
        let mut result = Vec::with_capacity(header_len + ciphertext.len());
        result.extend_from_slice(&packet[..header_len]);
        result.extend_from_slice(&ciphertext);
        Ok(result)
    }

    /// Encrypt an RTCP packet to SRTCP
    ///
    /// Implements RFC 3711 Section 3.4 SRTCP packet processing
    pub fn protect_rtcp(&mut self, packet: &[u8]) -> Result<Vec<u8>> {
        // Passthrough mode if no keys configured
        let Some(ref key_material) = self.local_key else {
            return Ok(packet.to_vec());
        };

        // RTCP packets must be at least 8 bytes (header + SSRC)
        if packet.len() < 8 {
            return Err(ForgeError::Srtp("RTCP packet too short".to_string()));
        }

        // Extract RTCP header fields
        let ssrc = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);

        // SRTCP uses its own index counter (separate from RTP ROC)
        let srtcp_index = self.local_srtcp_index & 0x7FFF_FFFF;
        self.local_srtcp_index = self.local_srtcp_index.wrapping_add(1) & 0x7FFF_FFFF;

        // Derive session keys using SRTCP index
        let derived_keys = key_material.derive_srtcp_session_keys(ssrc, srtcp_index as u64)?;

        // Encrypt based on profile
        let srtcp_packet = match key_material.profile {
            SrtpProfile::Aes128CmHmacSha1_80 | SrtpProfile::Aes128CmHmacSha1_32 => self
                .protect_rtcp_aes_cm(
                    packet,
                    &derived_keys,
                    ssrc,
                    srtcp_index,
                    key_material.profile,
                )?,
            SrtpProfile::AeadAes128Gcm | SrtpProfile::AeadAes256Gcm => self.protect_rtcp_aes_gcm(
                packet,
                &derived_keys,
                ssrc,
                srtcp_index,
                key_material.profile,
            )?,
        };

        // Increment metrics counter
        counter!("forge_srtcp_packets_encrypted_total", 1);

        Ok(srtcp_packet)
    }

    /// Protect RTCP packet using AES-CM + HMAC-SHA1
    fn protect_rtcp_aes_cm(
        &self,
        packet: &[u8],
        keys: &DerivedKeys,
        ssrc: u32,
        srtcp_index: u32,
        profile: SrtpProfile,
    ) -> Result<Vec<u8>> {
        // Construct IV for AES-CTR
        let mut iv = [0u8; 16];
        iv[..keys.salt.len()].copy_from_slice(&keys.salt);

        // XOR with SSRC (bytes 4-7)
        let ssrc_bytes = ssrc.to_be_bytes();
        for i in 0..4 {
            iv[4 + i] ^= ssrc_bytes[i];
        }

        // XOR with SRTCP index at bytes 10-13 (32 bits, big-endian MSB at 10).
        // Per RFC 3711 §4.1.2: IV = (k_s * 2^16) XOR (SSRC * 2^64) XOR (i * 2^16).
        // SRTCP index << 16 occupies bits 16..47 of the 128-bit IV, i.e.
        // bytes 10..13 in big-endian byte order.
        let index_bytes = srtcp_index.to_be_bytes();
        for i in 0..4 {
            iv[10 + i] ^= index_bytes[i];
        }

        // Encrypt payload (skip 8-byte header)
        let cipher = Aes128::new_from_slice(&keys.encryption_key)
            .map_err(|e| ForgeError::Srtp(format!("Failed to create AES cipher: {}", e)))?;

        let mut encrypted = packet.to_vec();
        let payload = &mut encrypted[8..]; // RTCP payload starts after 8-byte header

        let mut counter_block = iv;
        let mut offset = 0;

        while offset < payload.len() {
            let mut block = aes::Block::from(counter_block);
            cipher.encrypt_block(&mut block);

            let copy_len = (payload.len() - offset).min(16);
            for i in 0..copy_len {
                payload[offset + i] ^= block[i];
            }

            offset += 16;

            for j in (0..16).rev() {
                counter_block[j] = counter_block[j].wrapping_add(1);
                if counter_block[j] != 0 {
                    break;
                }
            }
        }

        // Append E-bit + SRTCP index (31-bit index + 1-bit E flag)
        let e_bit = 1u32 << 31; // E=1 means encrypted
        let srtcp_index_field = e_bit | srtcp_index;
        encrypted.extend_from_slice(&srtcp_index_field.to_be_bytes());

        // Compute HMAC-SHA1 over: encrypted packet + E|index
        let auth_key = keys
            .authentication_key
            .as_ref()
            .ok_or_else(|| ForgeError::Srtp("Missing authentication key".to_string()))?;

        let mut mac = <HmacSha1 as Mac>::new_from_slice(auth_key)
            .map_err(|e| ForgeError::Srtp(format!("Failed to create HMAC: {}", e)))?;

        mac.update(&encrypted);

        let auth_tag = mac.finalize().into_bytes();
        let tag_len = profile.auth_tag_len();

        // Append auth tag
        encrypted.extend_from_slice(&auth_tag[..tag_len]);

        Ok(encrypted)
    }

    /// Protect RTCP packet using AES-GCM AEAD
    ///
    /// Per RFC 7714, the AAD consists of the 8-byte RTCP header + E|index field.
    /// The auth tag follows the E|index in the output packet.
    fn protect_rtcp_aes_gcm(
        &self,
        packet: &[u8],
        keys: &DerivedKeys,
        ssrc: u32,
        srtcp_index: u32,
        profile: SrtpProfile,
    ) -> Result<Vec<u8>> {
        // Construct nonce (96 bits)
        let mut nonce_bytes = [0u8; 12];
        nonce_bytes[..keys.salt.len()].copy_from_slice(&keys.salt);

        // XOR with SSRC (bytes 2-5)
        let ssrc_bytes = ssrc.to_be_bytes();
        for i in 0..4 {
            nonce_bytes[2 + i] ^= ssrc_bytes[i];
        }

        // XOR with SRTCP index (bytes 8-11)
        let index_bytes = srtcp_index.to_be_bytes();
        for i in 0..4 {
            nonce_bytes[8 + i] ^= index_bytes[i];
        }

        let nonce = Nonce::from_slice(&nonce_bytes);

        // Compute E-bit + SRTCP index field (to include in AAD per RFC 7714)
        let e_bit = 1u32 << 31;
        let srtcp_index_field = e_bit | srtcp_index;
        let e_index_bytes = srtcp_index_field.to_be_bytes();

        // Build AAD: 8-byte header + E|index (RFC 7714 Section 8.3)
        let mut aad = Vec::with_capacity(12);
        aad.extend_from_slice(&packet[..8]);
        aad.extend_from_slice(&e_index_bytes);

        // Prepare payload for encryption
        let mut payload = packet[8..].to_vec();

        // Encrypt and get tag separately (per RFC 7714, tag follows E|index)
        let tag = match profile {
            SrtpProfile::AeadAes128Gcm => {
                let cipher = Aes128Gcm::new_from_slice(&keys.encryption_key).map_err(|e| {
                    ForgeError::Srtp(format!("Failed to create AES-GCM cipher: {}", e))
                })?;

                cipher
                    .encrypt_in_place_detached(nonce, &aad, &mut payload)
                    .map_err(|e| ForgeError::Srtp(format!("AES-GCM encryption failed: {}", e)))?
            }
            SrtpProfile::AeadAes256Gcm => {
                let cipher = Aes256Gcm::new_from_slice(&keys.encryption_key).map_err(|e| {
                    ForgeError::Srtp(format!("Failed to create AES-GCM cipher: {}", e))
                })?;

                cipher
                    .encrypt_in_place_detached(nonce, &aad, &mut payload)
                    .map_err(|e| ForgeError::Srtp(format!("AES-GCM encryption failed: {}", e)))?
            }
            _ => unreachable!(),
        };

        // Build result per RFC 7714: header + encrypted_payload + E|index + auth_tag
        let mut result = Vec::with_capacity(packet.len() + 4 + 16);
        result.extend_from_slice(&packet[..8]); // Header
        result.extend_from_slice(&payload); // Encrypted payload (without tag)
        result.extend_from_slice(&e_index_bytes); // E|index
        result.extend_from_slice(&tag); // Auth tag

        Ok(result)
    }

    /// Decrypt an SRTCP packet to RTCP
    ///
    /// Implements RFC 3711 Section 3.4 SRTCP packet processing
    pub fn unprotect_rtcp(&mut self, packet: &[u8]) -> Result<Vec<u8>> {
        // Passthrough mode if no keys configured
        let Some(ref key_material) = self.remote_key else {
            return Ok(packet.to_vec());
        };

        // SRTCP packet must have at least: 8-byte header + 4-byte E|index + auth tag
        let min_len = 8 + 4 + key_material.profile.auth_tag_len();
        if packet.len() < min_len {
            return Err(ForgeError::Srtp("SRTCP packet too short".to_string()));
        }

        let auth_tag_len = key_material.profile.auth_tag_len();

        // Extract E|index (last 4 bytes before auth tag)
        let e_index_offset = packet.len() - auth_tag_len - 4;
        let e_index = u32::from_be_bytes([
            packet[e_index_offset],
            packet[e_index_offset + 1],
            packet[e_index_offset + 2],
            packet[e_index_offset + 3],
        ]);

        let e_bit = (e_index >> 31) & 0x01;
        let srtcp_index = e_index & 0x7FFFFFFF;

        if e_bit == 0 {
            return Err(ForgeError::Srtp(
                "SRTCP packet not encrypted (E=0)".to_string(),
            ));
        }

        // SRTCP replay protection (RFC 3711 Section 3.4)
        // Check if this index has been seen before
        if self.srtcp_replay_window.check(srtcp_index as u64) {
            counter!("forge_srtcp_replay_attacks_blocked_total", 1);
            return Err(ForgeError::Srtp("SRTCP replay attack detected".to_string()));
        }

        // Extract SSRC
        let ssrc = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);

        // Derive session keys
        let derived_keys = key_material.derive_srtcp_session_keys(ssrc, srtcp_index as u64)?;

        // Decrypt based on profile
        let rtcp_packet = match key_material.profile {
            SrtpProfile::Aes128CmHmacSha1_80 | SrtpProfile::Aes128CmHmacSha1_32 => self
                .unprotect_rtcp_aes_cm(
                    packet,
                    &derived_keys,
                    ssrc,
                    srtcp_index,
                    key_material.profile,
                )?,
            SrtpProfile::AeadAes128Gcm | SrtpProfile::AeadAes256Gcm => self
                .unprotect_rtcp_aes_gcm(
                    packet,
                    &derived_keys,
                    ssrc,
                    srtcp_index,
                    key_material.profile,
                )?,
        };

        // Update SRTCP replay window and highest index
        self.srtcp_replay_window.update(srtcp_index as u64);
        if srtcp_index > self.remote_srtcp_index {
            self.remote_srtcp_index = srtcp_index;
        }

        // Increment metrics counter
        counter!("forge_srtcp_packets_decrypted_total", 1);

        Ok(rtcp_packet)
    }

    /// Unprotect SRTCP packet using AES-CM + HMAC-SHA1
    fn unprotect_rtcp_aes_cm(
        &self,
        packet: &[u8],
        keys: &DerivedKeys,
        ssrc: u32,
        srtcp_index: u32,
        profile: SrtpProfile,
    ) -> Result<Vec<u8>> {
        let tag_len = profile.auth_tag_len();
        let encrypted_len = packet.len() - tag_len;
        let encrypted_packet = &packet[..encrypted_len];
        let received_tag = &packet[encrypted_len..];

        // Verify HMAC-SHA1
        let auth_key = keys
            .authentication_key
            .as_ref()
            .ok_or_else(|| ForgeError::Srtp("Missing authentication key".to_string()))?;

        let mut mac = <HmacSha1 as Mac>::new_from_slice(auth_key)
            .map_err(|e| ForgeError::Srtp(format!("Failed to create HMAC: {}", e)))?;

        mac.update(encrypted_packet);

        let computed_tag = mac.finalize().into_bytes();

        if !bool::from(computed_tag[..tag_len].ct_eq(received_tag)) {
            return Err(ForgeError::Srtp("SRTCP authentication failed".to_string()));
        }

        // Remove E|index field (last 4 bytes before auth tag)
        let payload_end = encrypted_len - 4;
        let mut decrypted = encrypted_packet[..payload_end].to_vec();

        // Construct IV
        let mut iv = [0u8; 16];
        iv[..keys.salt.len()].copy_from_slice(&keys.salt);

        let ssrc_bytes = ssrc.to_be_bytes();
        for i in 0..4 {
            iv[4 + i] ^= ssrc_bytes[i];
        }

        // SRTCP index at bytes 10-13 (RFC 3711 §4.1.2: i << 16 → bits 16..47).
        let index_bytes = srtcp_index.to_be_bytes();
        for i in 0..4 {
            iv[10 + i] ^= index_bytes[i];
        }

        // Decrypt payload (skip 8-byte header)
        let cipher = Aes128::new_from_slice(&keys.encryption_key)
            .map_err(|e| ForgeError::Srtp(format!("Failed to create AES cipher: {}", e)))?;

        let payload = &mut decrypted[8..];
        let mut counter_block = iv;
        let mut offset = 0;

        while offset < payload.len() {
            let mut block = aes::Block::from(counter_block);
            cipher.encrypt_block(&mut block);

            let copy_len = (payload.len() - offset).min(16);
            for i in 0..copy_len {
                payload[offset + i] ^= block[i];
            }

            offset += 16;

            for j in (0..16).rev() {
                counter_block[j] = counter_block[j].wrapping_add(1);
                if counter_block[j] != 0 {
                    break;
                }
            }
        }

        Ok(decrypted)
    }

    /// Unprotect SRTCP packet using AES-GCM AEAD
    ///
    /// Per RFC 7714, packet format is: [header][encrypted_payload][E|index][auth_tag]
    /// AAD consists of: [header][E|index]
    fn unprotect_rtcp_aes_gcm(
        &self,
        packet: &[u8],
        keys: &DerivedKeys,
        ssrc: u32,
        srtcp_index: u32,
        profile: SrtpProfile,
    ) -> Result<Vec<u8>> {
        // Extract auth tag (last 16 bytes)
        let tag_len = 16;
        if packet.len() < 8 + 4 + tag_len {
            return Err(ForgeError::Srtp(
                "SRTCP packet too short for AES-GCM".to_string(),
            ));
        }

        let tag_start = packet.len() - tag_len;
        let tag = &packet[tag_start..];

        // Extract E|index (4 bytes before tag)
        let e_index_start = tag_start - 4;
        let e_index_bytes = &packet[e_index_start..tag_start];

        // Build AAD: header + E|index (per RFC 7714)
        let mut aad = Vec::with_capacity(12);
        aad.extend_from_slice(&packet[..8]); // Header
        aad.extend_from_slice(e_index_bytes); // E|index

        // Extract encrypted payload (between header and E|index)
        let mut ciphertext = packet[8..e_index_start].to_vec();

        // Construct nonce
        let mut nonce_bytes = [0u8; 12];
        nonce_bytes[..keys.salt.len()].copy_from_slice(&keys.salt);

        let ssrc_bytes = ssrc.to_be_bytes();
        for i in 0..4 {
            nonce_bytes[2 + i] ^= ssrc_bytes[i];
        }

        let index_bytes = srtcp_index.to_be_bytes();
        for i in 0..4 {
            nonce_bytes[8 + i] ^= index_bytes[i];
        }

        let nonce = Nonce::from_slice(&nonce_bytes);

        // Decrypt and verify with detached tag
        use aes_gcm::Tag;

        // Tag is 16 bytes for both AES-128-GCM and AES-256-GCM
        let tag_array: &[u8; 16] = tag
            .try_into()
            .map_err(|_| ForgeError::Srtp("Invalid tag length".to_string()))?;
        let tag_obj = Tag::from_slice(tag_array);

        match profile {
            SrtpProfile::AeadAes128Gcm => {
                let cipher = Aes128Gcm::new_from_slice(&keys.encryption_key).map_err(|e| {
                    ForgeError::Srtp(format!("Failed to create AES-GCM cipher: {}", e))
                })?;

                cipher
                    .decrypt_in_place_detached(nonce, &aad, &mut ciphertext, tag_obj)
                    .map_err(|e| {
                        ForgeError::Srtp(format!(
                            "SRTCP AES-GCM decryption/verification failed: {}",
                            e
                        ))
                    })?;
            }
            SrtpProfile::AeadAes256Gcm => {
                let cipher = Aes256Gcm::new_from_slice(&keys.encryption_key).map_err(|e| {
                    ForgeError::Srtp(format!("Failed to create AES-GCM cipher: {}", e))
                })?;

                cipher
                    .decrypt_in_place_detached(nonce, &aad, &mut ciphertext, tag_obj)
                    .map_err(|e| {
                        ForgeError::Srtp(format!(
                            "SRTCP AES-GCM decryption/verification failed: {}",
                            e
                        ))
                    })?;
            }
            _ => unreachable!(),
        }

        // Return header + decrypted payload
        let mut result = Vec::with_capacity(8 + ciphertext.len());
        result.extend_from_slice(&packet[..8]);
        result.extend_from_slice(&ciphertext);
        Ok(result)
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

    // RFC 3711 Appendix B.3 Test Vectors

    /// Test key derivation with RFC 3711 test vectors
    #[test]
    fn test_rfc3711_key_derivation() {
        // RFC 3711 Appendix B.3 test vectors
        let master_key = hex::decode("E1F97A0D3E018BE0D64FA32C06DE4139").unwrap();
        let master_salt = hex::decode("0EC675AD498AFEEBB6960B3AABE6").unwrap();

        let profile = SrtpProfile::Aes128CmHmacSha1_80;
        let key_material = SrtpKeyMaterial::new(master_key, master_salt, profile).unwrap();

        // Test key derivation for SSRC 0x00000000, index 0
        let ssrc = 0x00000000u32;
        let index = 0u64;

        let keys = key_material.derive_srtp_session_keys(ssrc, index).unwrap();

        // Expected cipher key from RFC 3711
        let expected_cipher_key = hex::decode("C61E7A93744F39EE10734AFE3FF7A087").unwrap();
        assert_eq!(
            keys.encryption_key, expected_cipher_key,
            "Cipher key derivation failed"
        );

        // Expected auth key from RFC 3711
        let expected_auth_key = hex::decode("CEBE321F6FF7716B6FD4AB49AF256A156D38BAA4").unwrap();
        assert_eq!(
            keys.authentication_key.unwrap(),
            expected_auth_key,
            "Auth key derivation failed"
        );

        // Expected salt key from RFC 3711
        let expected_salt_key = hex::decode("30CBBC08863D8C85D49DB34A9AE1").unwrap();
        assert_eq!(keys.salt, expected_salt_key, "Salt derivation failed");
    }

    /// Regression test for the SRTCP-uses-SRTP-labels bug.
    ///
    /// RFC 3711 §4.3.3 defines distinct KDF labels for SRTP
    /// (0x00/0x01/0x02) and SRTCP (0x03/0x04/0x05). The pre-fix
    /// `derive_session_keys` always used the SRTP labels regardless
    /// of which protocol was calling it — meaning every SRTCP packet
    /// from a spec-correct peer (Twilio, FreeSWITCH, every WebRTC
    /// stack) was discarded with "SRTCP authentication failed"
    /// because their auth tag was computed against label 0x04 and
    /// ours against label 0x01.
    ///
    /// This test pins that the two key sets are distinct. If a
    /// future refactor collapses the two paths back to one set of
    /// labels, this test fails loudly with the wrong key bytes.
    #[test]
    fn srtp_and_srtcp_session_keys_differ_by_label() {
        let master_key = hex::decode("E1F97A0D3E018BE0D64FA32C06DE4139").unwrap();
        let master_salt = hex::decode("0EC675AD498AFEEBB6960B3AABE6").unwrap();
        let km = SrtpKeyMaterial::new(master_key, master_salt, SrtpProfile::Aes128CmHmacSha1_80)
            .unwrap();

        let srtp = km.derive_srtp_session_keys(0, 0).unwrap();
        let srtcp = km.derive_srtcp_session_keys(0, 0).unwrap();

        assert_ne!(
            srtp.encryption_key, srtcp.encryption_key,
            "SRTP and SRTCP encryption keys must differ — they're derived from \
             the same master with different KDF labels (0x00 vs 0x03)"
        );
        assert_ne!(
            srtp.authentication_key, srtcp.authentication_key,
            "SRTP and SRTCP auth keys must differ — labels 0x01 vs 0x04. \
             A peer's auth-tag check fails when both sides use different \
             label sets, which is the production symptom this test exists to catch."
        );
        assert_ne!(
            srtp.salt, srtcp.salt,
            "SRTP and SRTCP salts must differ — labels 0x02 vs 0x05"
        );
    }

    /// Test SRTP encryption/decryption round-trip
    #[test]
    fn test_srtp_encrypt_decrypt_roundtrip() {
        // Create test RTP packet
        let rtp_packet = create_test_rtp_packet(0x1234, 0xDECAFBAD, 0x12345678);

        // Create SRTP context with test keys
        let master_key = vec![0x2Bu8; 16];
        let master_salt = vec![0xF0u8; 14];
        let profile = SrtpProfile::Aes128CmHmacSha1_80;

        let local_key =
            SrtpKeyMaterial::new(master_key.clone(), master_salt.clone(), profile).unwrap();
        let remote_key = SrtpKeyMaterial::new(master_key, master_salt, profile).unwrap();

        let mut encrypt_ctx = SrtpContext::new();
        encrypt_ctx.set_local_key(local_key);

        let mut decrypt_ctx = SrtpContext::new();
        decrypt_ctx.set_remote_key(remote_key);

        // Encrypt
        let srtp_packet = encrypt_ctx.protect_rtp(&rtp_packet).unwrap();

        // SRTP packet should be longer (auth tag added)
        assert!(srtp_packet.len() > rtp_packet.len());

        // Decrypt
        let decrypted = decrypt_ctx.unprotect_rtp(&srtp_packet).unwrap();

        // Should match original
        assert_eq!(decrypted, rtp_packet);
    }

    /// Cross-check AES-CM SRTP IV construction against RFC 3711 §4.1.1
    /// (RTP) and §4.1.2 (SRTCP) by computing the IV independently
    /// using u128 arithmetic that mirrors the spec's algebraic formula:
    ///
    /// ```text
    /// IV = (k_s * 2^16) XOR (SSRC * 2^64) XOR (i * 2^16)
    /// ```
    ///
    /// The existing round-trip tests cannot catch a symmetric off-by-N
    /// in `iv[...]` indexing because the same wrong bytes are XOR'd on
    /// both sides — wrong matches wrong, plaintext is recovered, test
    /// passes, and the bug only surfaces against a spec-correct peer.
    /// This test computes the expected keystream from first principles.
    #[test]
    fn test_srtp_aes_cm_iv_matches_rfc3711_spec() {
        use aes::cipher::{BlockEncrypt, KeyInit};
        use aes::Aes128;

        let master_key = vec![0x2Bu8; 16];
        let master_salt = vec![0xF0u8; 14];
        let profile = SrtpProfile::Aes128CmHmacSha1_80;
        let key_material = SrtpKeyMaterial::new(master_key, master_salt, profile).unwrap();

        let ssrc: u32 = 0xCAFE_BABE;
        let packet_index: u64 = 0x0001_0203_0405;
        let derived = key_material
            .derive_srtp_session_keys(ssrc, packet_index)
            .unwrap();

        // Spec-literal IV: salt is 112 bits, IV is 128 bits, so
        // (k_s * 2^16) left-aligns the salt in the IV.
        let salt_u128: u128 = derived
            .salt
            .iter()
            .fold(0u128, |acc, &b| (acc << 8) | b as u128);
        let iv_u128: u128 =
            (salt_u128 << 16) ^ ((ssrc as u128) << 64) ^ ((packet_index as u128) << 16);
        let expected_iv: [u8; 16] = iv_u128.to_be_bytes();

        // First keystream block under AES-CM.
        let cipher = Aes128::new_from_slice(&derived.encryption_key).unwrap();
        let mut block = aes::Block::clone_from_slice(&expected_iv);
        cipher.encrypt_block(&mut block);
        let expected_ks0: [u8; 16] = block.into();

        // Drive protect_aes_cm directly with a 16-byte zero payload.
        // XOR with zero leaves the keystream visible in the ciphertext.
        let mut packet = vec![
            0x80, 0x00, // V=2, P=0, X=0, CC=0, M=0, PT=0
            0x04, 0x05, // sequence (matches packet_index low 16 bits)
            0x00, 0x00, 0x00, 0x00, // timestamp (irrelevant for IV)
        ];
        packet.extend_from_slice(&ssrc.to_be_bytes());
        packet.extend_from_slice(&[0u8; 16]); // 16 bytes of zero payload

        let ctx = SrtpContext::new();
        let out = ctx
            .protect_aes_cm(
                &packet,
                12,
                &derived,
                ssrc,
                0x0001_0203,
                packet_index,
                profile,
            )
            .unwrap();

        // First 16 bytes of payload = AES-CM keystream (since plaintext is zero).
        let ciphertext_ks0 = &out[12..28];
        assert_eq!(
            ciphertext_ks0, &expected_ks0,
            "AES-CM keystream must match spec-literal IV; off-by-N in iv[...] \
             indexing will fail this test against any spec-correct peer"
        );
    }

    /// Cross-check AES-CM SRTCP IV construction against RFC 3711 §4.1.2.
    /// SRTCP_INDEX is 32 bits → bits 16..47 of the IV → bytes 10-13.
    #[test]
    fn test_srtcp_aes_cm_iv_matches_rfc3711_spec() {
        use aes::cipher::{BlockEncrypt, KeyInit};
        use aes::Aes128;

        let master_key = vec![0x3Cu8; 16];
        let master_salt = vec![0xE1u8; 14];
        let profile = SrtpProfile::Aes128CmHmacSha1_80;
        let key_material = SrtpKeyMaterial::new(master_key, master_salt, profile).unwrap();

        let ssrc: u32 = 0x1234_5678;
        let srtcp_index: u32 = 0x0001_0203;
        // SRTCP uses its own KDF labels; we exercise the same code path.
        let derived = key_material
            .derive_srtcp_session_keys(ssrc, srtcp_index as u64)
            .unwrap();

        let salt_u128: u128 = derived
            .salt
            .iter()
            .fold(0u128, |acc, &b| (acc << 8) | b as u128);
        let iv_u128: u128 =
            (salt_u128 << 16) ^ ((ssrc as u128) << 64) ^ ((srtcp_index as u128) << 16);
        let expected_iv: [u8; 16] = iv_u128.to_be_bytes();

        let cipher = Aes128::new_from_slice(&derived.encryption_key).unwrap();
        let mut block = aes::Block::clone_from_slice(&expected_iv);
        cipher.encrypt_block(&mut block);
        let expected_ks0: [u8; 16] = block.into();

        // Minimal SR packet: 8-byte header + 16-byte zero payload.
        let mut packet = vec![0x80, 0xC8, 0x00, 0x06]; // V=2, PT=SR(200), len
        packet.extend_from_slice(&ssrc.to_be_bytes());
        packet.extend_from_slice(&[0u8; 16]);

        let ctx = SrtpContext::new();
        let out = ctx
            .protect_rtcp_aes_cm(&packet, &derived, ssrc, srtcp_index, profile)
            .unwrap();

        // SRTCP encrypts everything after the 8-byte header.
        let ciphertext_ks0 = &out[8..24];
        assert_eq!(
            ciphertext_ks0, &expected_ks0,
            "SRTCP AES-CM keystream must match spec-literal IV"
        );
    }

    /// Test AES-GCM SRTP encryption/decryption
    #[test]
    fn test_srtp_aes_gcm_roundtrip() {
        let rtp_packet = create_test_rtp_packet(0x5678, 0xABCDEF00, 0x87654321);

        let master_key = vec![0xAAu8; 16];
        let master_salt = vec![0x55u8; 12];
        let profile = SrtpProfile::AeadAes128Gcm;

        let local_key =
            SrtpKeyMaterial::new(master_key.clone(), master_salt.clone(), profile).unwrap();
        let remote_key = SrtpKeyMaterial::new(master_key, master_salt, profile).unwrap();

        let mut encrypt_ctx = SrtpContext::new();
        encrypt_ctx.set_local_key(local_key);

        let mut decrypt_ctx = SrtpContext::new();
        decrypt_ctx.set_remote_key(remote_key);

        // Encrypt
        let srtp_packet = encrypt_ctx.protect_rtp(&rtp_packet).unwrap();
        assert!(srtp_packet.len() > rtp_packet.len());

        // Decrypt
        let decrypted = decrypt_ctx.unprotect_rtp(&srtp_packet).unwrap();
        assert_eq!(decrypted, rtp_packet);
    }

    /// Test SRTCP encryption/decryption round-trip
    #[test]
    fn test_srtcp_encrypt_decrypt_roundtrip() {
        // Create test RTCP packet (Sender Report)
        let rtcp_packet = create_test_rtcp_packet(0x12345678);

        let master_key = vec![0x3Cu8; 16];
        let master_salt = vec![0xC3u8; 14];
        let profile = SrtpProfile::Aes128CmHmacSha1_80;

        let local_key =
            SrtpKeyMaterial::new(master_key.clone(), master_salt.clone(), profile).unwrap();
        let remote_key = SrtpKeyMaterial::new(master_key, master_salt, profile).unwrap();

        let mut encrypt_ctx = SrtpContext::new();
        encrypt_ctx.set_local_key(local_key);

        let mut decrypt_ctx = SrtpContext::new();
        decrypt_ctx.set_remote_key(remote_key);

        // Encrypt
        let srtcp_packet = encrypt_ctx.protect_rtcp(&rtcp_packet).unwrap();

        // SRTCP packet should have E|index + auth tag
        assert!(srtcp_packet.len() > rtcp_packet.len() + 4);

        // Decrypt
        let decrypted = decrypt_ctx.unprotect_rtcp(&srtcp_packet).unwrap();
        assert_eq!(decrypted, rtcp_packet);
    }

    /// Test authentication failure detection
    #[test]
    fn test_srtp_authentication_failure() {
        let rtp_packet = create_test_rtp_packet(0x1111, 0x22222222, 0x33333333);

        let master_key = vec![0x42u8; 16];
        let master_salt = vec![0x24u8; 14];
        let profile = SrtpProfile::Aes128CmHmacSha1_80;

        let local_key =
            SrtpKeyMaterial::new(master_key.clone(), master_salt.clone(), profile).unwrap();
        let remote_key = SrtpKeyMaterial::new(master_key, master_salt, profile).unwrap();

        let mut encrypt_ctx = SrtpContext::new();
        encrypt_ctx.set_local_key(local_key);

        let mut decrypt_ctx = SrtpContext::new();
        decrypt_ctx.set_remote_key(remote_key);

        // Encrypt
        let mut srtp_packet = encrypt_ctx.protect_rtp(&rtp_packet).unwrap();

        // Tamper with the packet (flip a bit in the payload)
        if srtp_packet.len() > 15 {
            srtp_packet[15] ^= 0xFF;
        }

        // Decryption should fail due to authentication failure
        let result = decrypt_ctx.unprotect_rtp(&srtp_packet);
        assert!(result.is_err());
    }

    /// Test replay protection
    #[test]
    fn test_srtp_replay_protection() {
        let rtp_packet = create_test_rtp_packet(0x9999, 0xAAAAAAAA, 0xBBBBBBBB);

        let master_key = vec![0x77u8; 16];
        let master_salt = vec![0x88u8; 14];
        let profile = SrtpProfile::Aes128CmHmacSha1_80;

        let local_key =
            SrtpKeyMaterial::new(master_key.clone(), master_salt.clone(), profile).unwrap();
        let remote_key = SrtpKeyMaterial::new(master_key, master_salt, profile).unwrap();

        let mut encrypt_ctx = SrtpContext::new();
        encrypt_ctx.set_local_key(local_key);

        let mut decrypt_ctx = SrtpContext::new();
        decrypt_ctx.set_remote_key(remote_key);

        // Encrypt
        let srtp_packet = encrypt_ctx.protect_rtp(&rtp_packet).unwrap();

        // First decryption should succeed
        let result = decrypt_ctx.unprotect_rtp(&srtp_packet);
        assert!(result.is_ok());

        // Second decryption of same packet should fail (replay)
        let result = decrypt_ctx.unprotect_rtp(&srtp_packet);
        assert!(result.is_err());
    }

    /// Test ROC (Rollover Counter) wrapping
    #[test]
    fn test_roc_sequence_wrap() {
        let mut tracker = RocTracker::new();

        // Start near wrap point
        tracker.update(65530);
        assert_eq!(tracker.roc, 0);
        assert_eq!(tracker.highest_seq, 65530);

        // Progress towards wrap
        tracker.update(65534);
        assert_eq!(tracker.roc, 0);
        assert_eq!(tracker.highest_seq, 65534);

        tracker.update(65535);
        assert_eq!(tracker.roc, 0);
        assert_eq!(tracker.highest_seq, 65535);

        // Wrap around to 0
        tracker.update(0);
        assert_eq!(tracker.roc, 1);
        assert_eq!(tracker.highest_seq, 0);

        // Continue after wrap
        tracker.update(1);
        assert_eq!(tracker.roc, 1);
        assert_eq!(tracker.highest_seq, 1);

        tracker.update(100);
        assert_eq!(tracker.roc, 1);
        assert_eq!(tracker.highest_seq, 100);
    }

    /// Test AES-256-GCM key derivation (verifies SRTP-001 fix)
    #[test]
    fn test_aes256_gcm_key_derivation() {
        // Create key material for AES-256-GCM
        let master_key = vec![0x42; 32]; // 256-bit key
        let master_salt = vec![0x43; 12]; // 96-bit salt
        let profile = SrtpProfile::AeadAes256Gcm;

        // This should NOT fail with "Invalid Length" error
        let key_material = SrtpKeyMaterial::new(master_key, master_salt, profile)
            .expect("Failed to create key material for AES-256-GCM");

        // Verify key derivation works (this internally calls aes_cm_prf with 32-byte key)
        let derived = key_material
            .derive_srtp_session_keys(0x12345678, 0)
            .expect("Failed to derive session keys for AES-256-GCM");

        // Verify derived key lengths
        assert_eq!(
            derived.encryption_key.len(),
            32,
            "AES-256 encryption key should be 32 bytes"
        );
        assert_eq!(derived.salt.len(), 12, "GCM salt should be 12 bytes");
        assert!(
            derived.authentication_key.is_none(),
            "AEAD mode should not have separate auth key"
        );
    }

    /// Test AES-256-GCM encrypt/decrypt roundtrip
    #[test]
    fn test_aes256_gcm_roundtrip() {
        let master_key = vec![0x42; 32];
        let master_salt = vec![0x43; 12];
        let key_material = SrtpKeyMaterial::new(
            master_key.clone(),
            master_salt.clone(),
            SrtpProfile::AeadAes256Gcm,
        )
        .expect("Failed to create AES-256-GCM key material");

        // Create encrypt and decrypt contexts
        let mut encrypt_ctx = SrtpContext::new();
        encrypt_ctx.set_local_key(key_material.clone());

        let mut decrypt_ctx = SrtpContext::new();
        decrypt_ctx.set_remote_key(key_material);

        // Create test packet
        let packet = create_test_rtp_packet(1, 1000, 0x12345678);
        let original_len = packet.len();

        // Encrypt
        let encrypted = encrypt_ctx
            .protect_rtp(&packet)
            .expect("Failed to encrypt with AES-256-GCM");
        assert!(
            encrypted.len() > original_len,
            "Encrypted packet should be longer (includes auth tag)"
        );

        // Decrypt
        let decrypted = decrypt_ctx
            .unprotect_rtp(&encrypted)
            .expect("Failed to decrypt with AES-256-GCM");
        assert_eq!(packet, decrypted, "Decrypted packet should match original");
    }

    // Helper functions for tests

    fn create_test_rtp_packet(sequence: u16, timestamp: u32, ssrc: u32) -> Vec<u8> {
        let mut packet = Vec::new();

        // RTP header: V=2, P=0, X=0, CC=0, M=0, PT=0
        packet.push(0x80); // V=2, P=0, X=0, CC=0
        packet.push(0x00); // M=0, PT=0

        // Sequence number
        packet.extend_from_slice(&sequence.to_be_bytes());

        // Timestamp
        packet.extend_from_slice(&timestamp.to_be_bytes());

        // SSRC
        packet.extend_from_slice(&ssrc.to_be_bytes());

        // Payload (some test data)
        packet.extend_from_slice(b"Hello, SRTP!");

        packet
    }

    fn create_test_rtcp_packet(ssrc: u32) -> Vec<u8> {
        let mut packet = Vec::new();

        // RTCP SR header: V=2, P=0, RC=0, PT=200 (SR)
        packet.push(0x80); // V=2, P=0, RC=0
        packet.push(200); // PT=200 (Sender Report)

        // Length (in 32-bit words - 1)
        packet.extend_from_slice(&1u16.to_be_bytes());

        // SSRC of sender
        packet.extend_from_slice(&ssrc.to_be_bytes());

        // Sender info (minimal)
        packet.extend_from_slice(&[0u8; 20]);

        packet
    }

    // C1 regression: a crafted RTP header with CC=15 and a huge extension
    // length field must be rejected by both protect and unprotect rather than
    // overflowing `header_len` arithmetic and bypassing the subsequent bounds
    // check (which could enable an out-of-bounds read in the crypto stage).
    #[test]
    fn test_protect_rejects_malicious_extension_length() {
        let mut ctx = SrtpContext::new();
        ctx.set_local_key(
            SrtpKeyMaterial::new(
                vec![0u8; 16],
                vec![0u8; 14],
                SrtpProfile::Aes128CmHmacSha1_80,
            )
            .unwrap(),
        );

        // V=2, P=0, X=1, CC=15 → first byte 0x9F
        // PT=0, marker=0 → second byte 0x00
        // Sequence, timestamp, SSRC (12 bytes total)
        // 15 CSRCs (60 bytes)
        // Extension header: profile (2B) + length_words=0xFFFF (2B)
        // Claimed extension bytes: 0xFFFF * 4 = 262140 — far beyond packet length.
        let mut pkt = vec![0x9F, 0x00];
        pkt.extend_from_slice(&[0, 0]); // seq
        pkt.extend_from_slice(&[0, 0, 0, 0]); // ts
        pkt.extend_from_slice(&[0, 0, 0, 1]); // ssrc
        pkt.extend_from_slice(&[0u8; 60]); // 15 CSRCs
        pkt.extend_from_slice(&[0xBE, 0xDE, 0xFF, 0xFF]); // ext header w/ bogus length

        let res = ctx.protect_rtp(&pkt);
        assert!(res.is_err(), "protect_rtp must reject malicious extension");
    }

    #[test]
    fn test_unprotect_rejects_malicious_extension_length() {
        let mut ctx = SrtpContext::new();
        ctx.set_remote_key(
            SrtpKeyMaterial::new(
                vec![0u8; 16],
                vec![0u8; 14],
                SrtpProfile::Aes128CmHmacSha1_80,
            )
            .unwrap(),
        );

        // Same layout as above.
        let mut pkt = vec![0x9F, 0x00];
        pkt.extend_from_slice(&[0, 0]);
        pkt.extend_from_slice(&[0, 0, 0, 0]);
        pkt.extend_from_slice(&[0, 0, 0, 1]);
        pkt.extend_from_slice(&[0u8; 60]);
        pkt.extend_from_slice(&[0xBE, 0xDE, 0xFF, 0xFF]);
        // Pad with a fake auth tag so any length-mis-handling would otherwise proceed.
        pkt.extend_from_slice(&[0u8; 10]);

        let res = ctx.unprotect_rtp(&pkt);
        assert!(
            res.is_err(),
            "unprotect_rtp must reject malicious extension"
        );
    }
}
