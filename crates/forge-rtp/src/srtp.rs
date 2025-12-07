//! SRTP encryption/decryption

/// SRTP profile
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SrtpProfile {
    Aes128CmHmacSha1_80,
    Aes128CmHmacSha1_32,
    AeadAes128Gcm,
    AeadAes256Gcm,
}

/// Placeholder for SRTP implementation
/// TODO: Implement full SRTP support
pub struct SrtpContext;
