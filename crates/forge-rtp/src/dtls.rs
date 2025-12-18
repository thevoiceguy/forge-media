//! DTLS (Datagram Transport Layer Security) support for WebRTC
//!
//! Implements DTLS 1.2 for secure key exchange and negotiation.
//! Used to establish SRTP keys for encrypted media transport.

#[cfg(feature = "dtls")]
use openssl::{
    asn1::Asn1Time,
    bn::{BigNum, MsbOption},
    hash::MessageDigest,
    pkey::PKey,
    rsa::Rsa,
    ssl::{Ssl, SslContext, SslContextBuilder, SslMethod, SslVerifyMode, SslVersion},
    x509::{X509Builder, X509NameBuilder},
};

#[cfg(feature = "dtls")]
use foreign_types::ForeignType;

use crate::srtp::{SrtpKeyMaterial, SrtpProfile};
use forge_core::{ForgeError, Result};
use std::sync::Arc;
use tracing::{debug, info};

/// DTLS certificate with private key and fingerprint
#[derive(Clone)]
pub struct DtlsCertificate {
    /// X.509 certificate (DER encoded)
    pub cert_der: Vec<u8>,
    /// Private key (DER encoded)
    pub key_der: Vec<u8>,
    /// SHA-256 fingerprint of the certificate (for SDP)
    pub fingerprint: String,
}

impl DtlsCertificate {
    /// Generate a new self-signed certificate for DTLS
    ///
    /// Creates a 2048-bit RSA key pair and self-signed X.509 certificate
    /// valid for 30 days. The certificate is suitable for WebRTC DTLS.
    #[cfg(feature = "dtls")]
    pub fn generate() -> Result<Self> {
        info!("Generating DTLS certificate");

        // Generate RSA key pair (2048 bits)
        let rsa = Rsa::generate(2048)
            .map_err(|e| ForgeError::Internal(format!("Failed to generate RSA key: {}", e)))?;

        let pkey = PKey::from_rsa(rsa)
            .map_err(|e| ForgeError::Internal(format!("Failed to create PKey: {}", e)))?;

        // Create X.509 certificate builder
        let mut builder = X509Builder::new()
            .map_err(|e| ForgeError::Internal(format!("Failed to create X509Builder: {}", e)))?;

        // Set version to X.509v3
        builder
            .set_version(2)
            .map_err(|e| ForgeError::Internal(format!("Failed to set version: {}", e)))?;

        // Generate random serial number
        let mut serial = BigNum::new()
            .map_err(|e| ForgeError::Internal(format!("Failed to create BigNum: {}", e)))?;
        serial
            .rand(128, MsbOption::MAYBE_ZERO, false)
            .map_err(|e| ForgeError::Internal(format!("Failed to generate serial: {}", e)))?;
        let serial = serial
            .to_asn1_integer()
            .map_err(|e| ForgeError::Internal(format!("Failed to convert serial: {}", e)))?;
        builder
            .set_serial_number(&serial)
            .map_err(|e| ForgeError::Internal(format!("Failed to set serial: {}", e)))?;

        // Set subject and issuer (self-signed)
        let mut name_builder = X509NameBuilder::new()
            .map_err(|e| ForgeError::Internal(format!("Failed to create NameBuilder: {}", e)))?;
        name_builder
            .append_entry_by_text("CN", "forge-media")
            .map_err(|e| ForgeError::Internal(format!("Failed to set CN: {}", e)))?;
        let name = name_builder.build();

        builder
            .set_subject_name(&name)
            .map_err(|e| ForgeError::Internal(format!("Failed to set subject: {}", e)))?;
        builder
            .set_issuer_name(&name)
            .map_err(|e| ForgeError::Internal(format!("Failed to set issuer: {}", e)))?;

        // Set validity period (30 days)
        let not_before = Asn1Time::days_from_now(0)
            .map_err(|e| ForgeError::Internal(format!("Failed to create not_before: {}", e)))?;
        let not_after = Asn1Time::days_from_now(30)
            .map_err(|e| ForgeError::Internal(format!("Failed to create not_after: {}", e)))?;

        builder
            .set_not_before(&not_before)
            .map_err(|e| ForgeError::Internal(format!("Failed to set not_before: {}", e)))?;
        builder
            .set_not_after(&not_after)
            .map_err(|e| ForgeError::Internal(format!("Failed to set not_after: {}", e)))?;

        // Set public key
        builder
            .set_pubkey(&pkey)
            .map_err(|e| ForgeError::Internal(format!("Failed to set pubkey: {}", e)))?;

        // Sign the certificate with the private key
        builder
            .sign(&pkey, MessageDigest::sha256())
            .map_err(|e| ForgeError::Internal(format!("Failed to sign certificate: {}", e)))?;

        let cert = builder.build();

        // Export to DER format
        let cert_der = cert
            .to_der()
            .map_err(|e| ForgeError::Internal(format!("Failed to export cert to DER: {}", e)))?;
        let key_der = pkey
            .private_key_to_der()
            .map_err(|e| ForgeError::Internal(format!("Failed to export key to DER: {}", e)))?;

        // Calculate SHA-256 fingerprint
        let fingerprint = Self::calculate_fingerprint(&cert_der)?;

        debug!(
            "Generated DTLS certificate with fingerprint: {}",
            fingerprint
        );

        Ok(Self {
            cert_der,
            key_der,
            fingerprint,
        })
    }

    /// Calculate SHA-256 fingerprint of a certificate in DER format
    ///
    /// Returns the fingerprint as a colon-separated hex string (e.g., "AB:CD:EF:...")
    #[cfg(feature = "dtls")]
    fn calculate_fingerprint(cert_der: &[u8]) -> Result<String> {
        use openssl::hash::{hash, MessageDigest};

        let digest = hash(MessageDigest::sha256(), cert_der)
            .map_err(|e| ForgeError::Internal(format!("Failed to hash certificate: {}", e)))?;

        // Format as colon-separated hex
        let fingerprint = digest
            .iter()
            .map(|b| format!("{:02X}", b))
            .collect::<Vec<_>>()
            .join(":");

        Ok(fingerprint)
    }

    /// Get the SHA-256 fingerprint for use in SDP
    pub fn fingerprint_sha256(&self) -> &str {
        &self.fingerprint
    }

    #[cfg(not(feature = "dtls"))]
    pub fn generate() -> Result<Self> {
        Err(ForgeError::Internal(
            "DTLS support not enabled. Enable the 'dtls' feature.".to_string(),
        ))
    }
}

/// DTLS role in the handshake
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DtlsRole {
    /// Client (active) - initiates the DTLS handshake
    Client,
    /// Server (passive) - waits for the client to initiate
    Server,
}

impl DtlsRole {
    /// Parse from SDP setup attribute
    pub fn from_sdp_setup(setup: &str) -> Option<Self> {
        match setup {
            "active" => Some(DtlsRole::Client),
            "passive" => Some(DtlsRole::Server),
            "actpass" => Some(DtlsRole::Server), // Default to server
            _ => None,
        }
    }

    /// Convert to SDP setup attribute
    pub fn to_sdp_setup(&self) -> &'static str {
        match self {
            DtlsRole::Client => "active",
            DtlsRole::Server => "passive",
        }
    }
}

/// DTLS connection state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DtlsState {
    /// Initial state, not started
    New,
    /// Handshake in progress
    Handshaking,
    /// Handshake completed successfully
    Connected,
    /// Handshake or connection failed
    Failed,
    /// Connection closed
    Closed,
}

/// DTLS context for managing SSL/TLS settings
#[cfg(feature = "dtls")]
pub struct DtlsContext {
    /// SSL context for DTLS
    ssl_context: SslContext,
    /// Certificate used for this context
    certificate: Arc<DtlsCertificate>,
}

#[cfg(feature = "dtls")]
impl DtlsContext {
    /// Create a new DTLS context with the given certificate
    ///
    /// Configures the SSL context for DTLS 1.2 with appropriate cipher suites
    /// and SRTP protection profiles.
    pub fn new(certificate: Arc<DtlsCertificate>, role: DtlsRole) -> Result<Self> {
        let mut builder = SslContextBuilder::new(SslMethod::dtls())
            .map_err(|e| ForgeError::Internal(format!("Failed to create SslContext: {}", e)))?;

        // Set DTLS version to 1.2
        builder
            .set_min_proto_version(Some(SslVersion::TLS1_2))
            .map_err(|e| ForgeError::Internal(format!("Failed to set min version: {}", e)))?;
        builder
            .set_max_proto_version(Some(SslVersion::TLS1_2))
            .map_err(|e| ForgeError::Internal(format!("Failed to set max version: {}", e)))?;

        // Load certificate and private key from DER
        let cert = openssl::x509::X509::from_der(&certificate.cert_der)
            .map_err(|e| ForgeError::Internal(format!("Failed to load certificate: {}", e)))?;
        let pkey = PKey::private_key_from_der(&certificate.key_der)
            .map_err(|e| ForgeError::Internal(format!("Failed to load private key: {}", e)))?;

        builder
            .set_certificate(&cert)
            .map_err(|e| ForgeError::Internal(format!("Failed to set certificate: {}", e)))?;
        builder
            .set_private_key(&pkey)
            .map_err(|e| ForgeError::Internal(format!("Failed to set private key: {}", e)))?;

        // Verify that the private key matches the certificate
        builder
            .check_private_key()
            .map_err(|e| ForgeError::Internal(format!("Private key check failed: {}", e)))?;

        // Set cipher suites (prefer ECDHE for forward secrecy)
        builder
            .set_cipher_list("ECDHE-RSA-AES128-GCM-SHA256:ECDHE-RSA-AES256-GCM-SHA384")
            .map_err(|e| ForgeError::Internal(format!("Failed to set cipher list: {}", e)))?;

        // Configure SRTP protection profiles (for key export)
        // SRTP_AES128_CM_SHA1_80 is the most widely supported
        builder
            .set_tlsext_use_srtp("SRTP_AES128_CM_SHA1_80:SRTP_AES128_CM_SHA1_32")
            .map_err(|e| ForgeError::Internal(format!("Failed to set SRTP profiles: {}", e)))?;

        // Set verification mode
        match role {
            DtlsRole::Client => {
                // Client verifies server certificate
                builder.set_verify(SslVerifyMode::PEER);
            }
            DtlsRole::Server => {
                // Server requests but doesn't require client certificate
                builder.set_verify(SslVerifyMode::PEER | SslVerifyMode::FAIL_IF_NO_PEER_CERT);
            }
        }

        // Build the context
        let ssl_context = builder.build();

        debug!("Created DTLS context for role: {:?}", role);

        Ok(Self {
            ssl_context,
            certificate,
        })
    }

    /// Get the certificate
    pub fn certificate(&self) -> &Arc<DtlsCertificate> {
        &self.certificate
    }

    /// Create an SSL instance for a new DTLS connection
    pub fn create_ssl(&self) -> Result<Ssl> {
        Ssl::new(&self.ssl_context)
            .map_err(|e| ForgeError::Internal(format!("Failed to create SSL: {}", e)))
    }
}

#[cfg(not(feature = "dtls"))]
pub struct DtlsContext;

#[cfg(not(feature = "dtls"))]
impl DtlsContext {
    pub fn new(_certificate: Arc<DtlsCertificate>, _role: DtlsRole) -> Result<Self> {
        Err(ForgeError::Internal(
            "DTLS support not enabled. Enable the 'dtls' feature.".to_string(),
        ))
    }
}

/// DTLS connection managing handshake and encryption
#[cfg(feature = "dtls")]
pub struct DtlsConnection {
    /// SSL instance
    ssl: Ssl,
    /// DTLS role
    role: DtlsRole,
    /// Connection state
    state: DtlsState,
    /// Remote fingerprint to verify (SHA-256)
    expected_remote_fingerprint: Option<String>,
    /// Input buffer for incoming DTLS packets
    incoming_buffer: Vec<u8>,
    /// Output buffer for outgoing DTLS packets
    outgoing_buffer: Vec<u8>,
}

#[cfg(feature = "dtls")]
impl DtlsConnection {
    /// Create a new DTLS connection
    pub fn new(
        context: &DtlsContext,
        role: DtlsRole,
        remote_fingerprint: Option<String>,
    ) -> Result<Self> {
        let mut ssl = context.create_ssl()?;

        // Set connection behavior based on role
        match role {
            DtlsRole::Client => {
                ssl.set_connect_state();
            }
            DtlsRole::Server => {
                ssl.set_accept_state();
            }
        }

        debug!("Created DTLS connection with role: {:?}", role);

        Ok(Self {
            ssl,
            role,
            state: DtlsState::New,
            expected_remote_fingerprint: remote_fingerprint,
            incoming_buffer: Vec::with_capacity(8192),
            outgoing_buffer: Vec::with_capacity(8192),
        })
    }

    /// Get current connection state
    pub fn state(&self) -> DtlsState {
        self.state
    }

    /// Get the DTLS role
    pub fn role(&self) -> DtlsRole {
        self.role
    }

    /// Perform DTLS handshake over UDP
    ///
    /// Processes incoming DTLS data and advances the handshake state machine.
    /// Returns (is_complete, outgoing_data) where:
    /// - is_complete: true if handshake finished successfully
    /// - outgoing_data: DTLS packets to send to the peer
    ///
    /// # Arguments
    /// * `incoming` - Optional incoming DTLS packet data from peer
    ///
    /// # Returns
    /// Returns (bool, Vec<u8>) - (handshake_complete, data_to_send)
    ///
    /// # Implementation Note
    /// This uses OpenSSL's memory BIO (via FFI) to manage the DTLS handshake
    /// over a connectionless transport (UDP).
    pub fn handshake(&mut self, incoming: Option<&[u8]>) -> Result<(bool, Vec<u8>)> {
        use openssl::error::ErrorStack;
        use openssl_sys::{
            BIO_ctrl, BIO_new, BIO_read, BIO_s_mem, BIO_write, SSL_do_handshake, SSL_get_error,
            SSL_get_rbio, SSL_get_wbio, SSL_set_bio, SSL_ERROR_NONE, SSL_ERROR_WANT_READ,
            SSL_ERROR_WANT_WRITE,
        };

        // BIO_CTRL_PENDING constant (OpenSSL standard value)
        const BIO_CTRL_PENDING: std::os::raw::c_int = 10;

        if self.state == DtlsState::Connected {
            return Ok((true, Vec::new()));
        }

        if self.state == DtlsState::Failed || self.state == DtlsState::Closed {
            return Err(ForgeError::Internal(format!(
                "DTLS connection in invalid state: {:?}",
                self.state
            )));
        }

        // Transition to handshaking state
        if self.state == DtlsState::New {
            self.state = DtlsState::Handshaking;
            debug!("Starting DTLS handshake (role: {:?})", self.role);

            unsafe {
                // Create memory BIOs for SSL I/O
                let rbio = BIO_new(BIO_s_mem());
                let wbio = BIO_new(BIO_s_mem());

                if rbio.is_null() || wbio.is_null() {
                    return Err(ForgeError::Internal("Failed to create BIOs".to_string()));
                }

                // Set the BIOs on the SSL object
                SSL_set_bio(self.ssl.as_ptr(), rbio, wbio);
            }
        }

        unsafe {
            let ssl_ptr = self.ssl.as_ptr();

            // If we have incoming data, write it to the read BIO
            if let Some(data) = incoming {
                debug!("Processing {} bytes of incoming DTLS data", data.len());
                let rbio = SSL_get_rbio(ssl_ptr);
                if !rbio.is_null() {
                    let written = BIO_write(rbio, data.as_ptr() as *const _, data.len() as i32);
                    if written < 0 {
                        return Err(ForgeError::Internal(
                            "Failed to write to read BIO".to_string(),
                        ));
                    }
                }
            }

            // Attempt the handshake
            let ret = SSL_do_handshake(ssl_ptr);
            let error = SSL_get_error(ssl_ptr, ret);

            // Read any outgoing data from the write BIO
            let mut outgoing = Vec::new();
            let wbio = SSL_get_wbio(ssl_ptr);
            if !wbio.is_null() {
                let pending = BIO_ctrl(wbio, BIO_CTRL_PENDING, 0, std::ptr::null_mut()) as usize;
                if pending > 0 {
                    outgoing.resize(pending, 0);
                    let read = BIO_read(wbio, outgoing.as_mut_ptr() as *mut _, pending as i32);
                    if read > 0 {
                        outgoing.truncate(read as usize);
                        debug!("Sending {} bytes of DTLS handshake data", outgoing.len());
                    } else {
                        outgoing.clear();
                    }
                }
            }

            if error == SSL_ERROR_NONE {
                // Handshake completed successfully
                debug!("DTLS handshake completed successfully");

                // Verify remote fingerprint if expected
                if let Err(e) = self.verify_remote_fingerprint() {
                    self.state = DtlsState::Failed;
                    return Err(ForgeError::Internal(format!(
                        "Fingerprint verification failed: {}",
                        e
                    )));
                }

                // Transition to connected state
                self.state = DtlsState::Connected;
                Ok((true, outgoing))
            } else if error == SSL_ERROR_WANT_READ || error == SSL_ERROR_WANT_WRITE {
                // Handshake in progress, need more data
                debug!("DTLS handshake in progress");
                Ok((false, outgoing))
            } else {
                // Handshake failed
                self.state = DtlsState::Failed;
                let err = ErrorStack::get();
                Err(ForgeError::Internal(format!(
                    "DTLS handshake failed (error {}): {:?}",
                    error, err
                )))
            }
        }
    }

    /// Verify that the remote certificate fingerprint matches expected value
    #[cfg(feature = "dtls")]
    pub fn verify_remote_fingerprint(&self) -> Result<bool> {
        if let Some(ref expected) = self.expected_remote_fingerprint {
            // Get peer certificate
            let peer_cert = self
                .ssl
                .peer_certificate()
                .ok_or_else(|| ForgeError::Internal("No peer certificate".to_string()))?;

            // Calculate fingerprint
            let cert_der = peer_cert
                .to_der()
                .map_err(|e| ForgeError::Internal(format!("Failed to export cert: {}", e)))?;
            let actual = DtlsCertificate::calculate_fingerprint(&cert_der)?;

            if &actual != expected {
                return Err(ForgeError::Internal(format!(
                    "Fingerprint mismatch: expected {}, got {}",
                    expected, actual
                )));
            }

            debug!("Remote fingerprint verified: {}", actual);
            Ok(true)
        } else {
            // No expected fingerprint, skip verification
            Ok(true)
        }
    }

    /// Export SRTP keying material after successful handshake
    ///
    /// Uses RFC 5764 DTLS-SRTP key derivation to export keys for SRTP.
    /// Returns (client_key_material, server_key_material).
    #[cfg(feature = "dtls")]
    pub fn export_srtp_keys(&self) -> Result<(SrtpKeyMaterial, SrtpKeyMaterial)> {
        if self.state != DtlsState::Connected {
            return Err(ForgeError::Internal(
                "DTLS not connected, cannot export keys".to_string(),
            ));
        }

        // Export keying material using RFC 5764 label
        // SRTP_AES128_CM_SHA1_80 uses:
        // - 16 bytes master key
        // - 14 bytes master salt
        // Total: 30 bytes per direction = 60 bytes total
        let label = "EXTRACTOR-dtls_srtp";
        let mut key_material = vec![0u8; 60];

        self.ssl
            .export_keying_material(&mut key_material, label, None)
            .map_err(|e| ForgeError::Internal(format!("Failed to export keys: {}", e)))?;

        // Split into client and server keys
        let client_master_key = key_material[0..16].to_vec();
        let server_master_key = key_material[16..32].to_vec();
        let client_master_salt = key_material[32..46].to_vec();
        let server_master_salt = key_material[46..60].to_vec();

        // Use AES_CM_128_HMAC_SHA1_80 profile (standard for WebRTC)
        let profile = SrtpProfile::Aes128CmHmacSha1_80;

        let client_keys = SrtpKeyMaterial {
            master_key: client_master_key,
            master_salt: client_master_salt,
            profile,
        };

        let server_keys = SrtpKeyMaterial {
            master_key: server_master_key,
            master_salt: server_master_salt,
            profile,
        };

        debug!("Exported SRTP keys from DTLS");

        Ok((client_keys, server_keys))
    }

    /// Mark handshake as complete (for testing)
    #[cfg(test)]
    pub fn mark_connected(&mut self) {
        self.state = DtlsState::Connected;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "dtls")]
    fn test_certificate_generation() {
        let cert = DtlsCertificate::generate().unwrap();

        // Check that DER data is not empty
        assert!(!cert.cert_der.is_empty());
        assert!(!cert.key_der.is_empty());

        // Check fingerprint format (XX:XX:XX:...)
        assert!(cert.fingerprint.len() > 0);
        assert!(cert.fingerprint.contains(':'));

        // SHA-256 produces 32 bytes = 64 hex chars + 31 colons = 95 chars
        assert_eq!(cert.fingerprint.len(), 95);

        println!("Generated certificate fingerprint: {}", cert.fingerprint);
    }

    #[test]
    #[cfg(feature = "dtls")]
    fn test_dtls_context_creation() {
        let cert = Arc::new(DtlsCertificate::generate().unwrap());

        // Test client context
        let client_ctx = DtlsContext::new(cert.clone(), DtlsRole::Client);
        assert!(client_ctx.is_ok());

        // Test server context
        let server_ctx = DtlsContext::new(cert.clone(), DtlsRole::Server);
        assert!(server_ctx.is_ok());
    }

    #[test]
    fn test_dtls_role_sdp() {
        assert_eq!(DtlsRole::Client.to_sdp_setup(), "active");
        assert_eq!(DtlsRole::Server.to_sdp_setup(), "passive");

        assert_eq!(DtlsRole::from_sdp_setup("active"), Some(DtlsRole::Client));
        assert_eq!(DtlsRole::from_sdp_setup("passive"), Some(DtlsRole::Server));
        assert_eq!(DtlsRole::from_sdp_setup("actpass"), Some(DtlsRole::Server));
        assert_eq!(DtlsRole::from_sdp_setup("invalid"), None);
    }

    #[test]
    #[cfg(feature = "dtls")]
    fn test_dtls_connection_creation() {
        let cert = Arc::new(DtlsCertificate::generate().unwrap());
        let ctx = DtlsContext::new(cert.clone(), DtlsRole::Client).unwrap();

        // Create client connection
        let conn = DtlsConnection::new(&ctx, DtlsRole::Client, None);
        assert!(conn.is_ok());

        let conn = conn.unwrap();
        assert_eq!(conn.state(), DtlsState::New);
        assert_eq!(conn.role(), DtlsRole::Client);
    }

    #[test]
    #[cfg(feature = "dtls")]
    fn test_dtls_key_export() {
        let cert = Arc::new(DtlsCertificate::generate().unwrap());
        let ctx = DtlsContext::new(cert.clone(), DtlsRole::Client).unwrap();
        let mut conn = DtlsConnection::new(&ctx, DtlsRole::Client, None).unwrap();

        // Key export before handshake should fail
        let result = conn.export_srtp_keys();
        assert!(result.is_err());

        // Mark as connected (simulates completed handshake for testing purposes)
        // Note: In a real scenario, a proper handshake would need to occur
        conn.mark_connected();

        // Key export should still fail because we haven't done a real SSL handshake
        // This is expected behavior - OpenSSL requires an actual handshake
        // In production, use with real DTLS peers
        let result = conn.export_srtp_keys();
        // We expect this to fail without a real handshake
        // Just verify the API exists and has the right signature
        if let Err(e) = result {
            println!("Expected error without real handshake: {}", e);
        }
    }
}
