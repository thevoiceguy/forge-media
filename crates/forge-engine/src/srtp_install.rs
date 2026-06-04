//! SRTP key installation, exchange-agnostic.
//!
//! Whether the master keys were derived from a DTLS handshake
//! (RFC 5764), a parsed `a=crypto:` attribute (RFC 4568 / SDES),
//! or any future exchange that bottoms out at
//! [`forge_rtp::srtp::SrtpKeyMaterial`], the install path is the
//! same: take ownership of the per-session `Arc<Mutex<SrtpContext>>`,
//! set the local (outbound-encryption) and remote (inbound-decryption)
//! keys, return.
//!
//! Factored out of [`crate::dtls_srtp::install_keys`] (which is
//! gated on the `dtls` feature) so the SDES path can call it without
//! pulling in OpenSSL. The DTLS path now delegates here.

use forge_rtp::srtp::{SrtpContext, SrtpKeyMaterial};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::debug;

/// Install derived SRTP keys into an existing [`SrtpContext`].
///
/// `local_srtp_key` is used for *outgoing* packets
/// ([`SrtpContext::set_local_key`]); `remote_srtp_key` is used for
/// *incoming* ([`SrtpContext::set_remote_key`]). The caller owns the
/// lifetime of the context — this function takes `&Arc<…>` and only
/// locks while writing the two key fields.
///
/// Exchange-agnostic: the caller is responsible for deriving the
/// key material correctly for their chosen exchange (DTLS-SRTP
/// per RFC 5764 §4.2, SDES per RFC 4568 §6.1, etc.).
pub async fn install_srtp_keys(
    srtp_ctx: &Arc<Mutex<SrtpContext>>,
    local_srtp_key: SrtpKeyMaterial,
    remote_srtp_key: SrtpKeyMaterial,
) {
    let mut ctx = srtp_ctx.lock().await;
    ctx.set_local_key(local_srtp_key);
    ctx.set_remote_key(remote_srtp_key);
    debug!(
        "SRTP keys installed into SrtpContext (enabled: {})",
        ctx.is_enabled()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_rtp::srtp::SrtpProfile;

    /// Smoke test: a context that started empty becomes enabled after
    /// installation. We use synthetic key material of the correct
    /// length; actual round-trip crypto coverage lives in
    /// `forge_rtp::srtp` tests.
    #[tokio::test]
    async fn install_enables_context() {
        let ctx = Arc::new(Mutex::new(SrtpContext::new()));
        assert!(!ctx.lock().await.is_enabled());

        let profile = SrtpProfile::Aes128CmHmacSha1_80;
        let local = SrtpKeyMaterial::new(
            vec![0u8; profile.master_key_len()],
            vec![0u8; profile.master_salt_len()],
            profile,
        )
        .expect("local key");
        let remote = SrtpKeyMaterial::new(
            vec![1u8; profile.master_key_len()],
            vec![1u8; profile.master_salt_len()],
            profile,
        )
        .expect("remote key");

        install_srtp_keys(&ctx, local, remote).await;
        assert!(ctx.lock().await.is_enabled());
    }
}
