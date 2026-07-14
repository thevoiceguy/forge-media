//! SDES (Session Description Protocol Security Descriptions) per RFC 4568
//!
//! Parses and generates the `a=crypto:` SDP attribute used to negotiate
//! SRTP master keys in-band on the SIP signalling plane. Supports the
//! AES-CM-128-HMAC-SHA1 family (RFC 4568) and the AEAD-AES-GCM family
//! (RFC 7714).
//!
//! ## Security note
//!
//! SDES exchanges the SRTP master key in the SDP body of SIP messages.
//! If the SIP signalling itself is not encrypted (plaintext UDP/TCP),
//! the key is exposed and SRTP gives no confidentiality. Operators MUST
//! pair SDES with SIP over TLS.
//!
//! ## Example
//!
//! ```no_run
//! use forge_sdp::sdes::{CryptoAttribute, CryptoSuite, answer_sdes};
//!
//! // Parse one of the offerer's a=crypto lines
//! let offered: CryptoAttribute = "a=crypto:1 AES_CM_128_HMAC_SHA1_80 \
//!     inline:PS1uQCVeeCFCanVmcjkpaywdyShmHKBs8oxnNvQAg2k="
//!     .parse()?;
//!
//! // Mint the local side and derive RX/TX key material in one shot
//! let answer = answer_sdes(&offered)?;
//! let answer_line = answer.local_attribute.to_attribute_line();
//! # Ok::<(), forge_sdp::sdes::SdesError>(())
//! ```

use crate::{Attribute, MediaDescription, SessionDescription};
use base64::{engine::general_purpose::STANDARD, Engine};
use forge_rtp::{SrtpKeyMaterial, SrtpProfile};
use rand::Rng;
use smol_str::SmolStr;
use std::str::FromStr;
use thiserror::Error;

/// Errors produced by SDES parsing.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SdesError {
    /// The crypto-attribute is missing a required field or otherwise malformed.
    #[error("malformed a=crypto attribute: {0}")]
    Malformed(String),

    /// The crypto-suite token is not one this crate recognises.
    #[error("unsupported crypto suite: {0}")]
    UnsupportedSuite(String),

    /// The tag is not a 1*9DIGIT token per RFC 4568 §9.2.
    #[error("invalid crypto tag: {0}")]
    InvalidTag(String),

    /// The inline base64 payload failed to decode.
    #[error("invalid base64 in key-info: {0}")]
    InvalidBase64(String),

    /// Key+salt length didn't match the suite's required length. SDES treats
    /// this as a hard failure — never silently truncate.
    #[error("key-info length {actual} does not match suite {suite} (expected {expected})")]
    KeyLengthMismatch {
        /// The suite that was claimed.
        suite: &'static str,
        /// Bytes the suite requires (master_key_len + master_salt_len).
        expected: usize,
        /// Bytes actually decoded.
        actual: usize,
    },

    /// An MKI clause failed to parse (`mki-value:mki-length`).
    #[error("invalid MKI clause: {0}")]
    InvalidMki(String),

    /// The key-method on a key-param was not `inline:` — the only one we support.
    #[error("unsupported key-method: {0}")]
    UnsupportedKeyMethod(String),
}

/// SRTP crypto suite as named on the wire by RFC 4568 / RFC 7714.
///
/// The wire token is the canonical form used in the `a=crypto:` attribute;
/// `as_str` and `FromStr` round-trip that token verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CryptoSuite {
    /// `AES_CM_128_HMAC_SHA1_80` — RFC 4568 §6.2.1. The Twilio default.
    Aes128CmHmacSha1_80,
    /// `AES_CM_128_HMAC_SHA1_32` — RFC 4568 §6.2.2.
    Aes128CmHmacSha1_32,
    /// `AEAD_AES_128_GCM` — RFC 7714 §14.
    AeadAes128Gcm,
    /// `AEAD_AES_256_GCM` — RFC 7714 §14.
    AeadAes256Gcm,
}

impl CryptoSuite {
    /// Wire token as it appears in `a=crypto:`.
    pub fn as_str(&self) -> &'static str {
        match self {
            CryptoSuite::Aes128CmHmacSha1_80 => "AES_CM_128_HMAC_SHA1_80",
            CryptoSuite::Aes128CmHmacSha1_32 => "AES_CM_128_HMAC_SHA1_32",
            CryptoSuite::AeadAes128Gcm => "AEAD_AES_128_GCM",
            CryptoSuite::AeadAes256Gcm => "AEAD_AES_256_GCM",
        }
    }

    /// Map to the corresponding `SrtpProfile` enum used by `forge-rtp`.
    pub fn to_srtp_profile(self) -> SrtpProfile {
        match self {
            CryptoSuite::Aes128CmHmacSha1_80 => SrtpProfile::Aes128CmHmacSha1_80,
            CryptoSuite::Aes128CmHmacSha1_32 => SrtpProfile::Aes128CmHmacSha1_32,
            CryptoSuite::AeadAes128Gcm => SrtpProfile::AeadAes128Gcm,
            CryptoSuite::AeadAes256Gcm => SrtpProfile::AeadAes256Gcm,
        }
    }

    /// Map back from `SrtpProfile`.
    pub fn from_srtp_profile(p: SrtpProfile) -> Self {
        match p {
            SrtpProfile::Aes128CmHmacSha1_80 => CryptoSuite::Aes128CmHmacSha1_80,
            SrtpProfile::Aes128CmHmacSha1_32 => CryptoSuite::Aes128CmHmacSha1_32,
            SrtpProfile::AeadAes128Gcm => CryptoSuite::AeadAes128Gcm,
            SrtpProfile::AeadAes256Gcm => CryptoSuite::AeadAes256Gcm,
        }
    }

    /// Required key+salt length for inline key-info (master_key_len + master_salt_len).
    pub fn key_info_len(self) -> usize {
        let p = self.to_srtp_profile();
        p.master_key_len() + p.master_salt_len()
    }
}

impl FromStr for CryptoSuite {
    type Err = SdesError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "AES_CM_128_HMAC_SHA1_80" => Ok(CryptoSuite::Aes128CmHmacSha1_80),
            "AES_CM_128_HMAC_SHA1_32" => Ok(CryptoSuite::Aes128CmHmacSha1_32),
            "AEAD_AES_128_GCM" => Ok(CryptoSuite::AeadAes128Gcm),
            "AEAD_AES_256_GCM" => Ok(CryptoSuite::AeadAes256Gcm),
            other => Err(SdesError::UnsupportedSuite(other.to_string())),
        }
    }
}

/// MKI (Master Key Identifier) clause from `mki-value:mki-length` per RFC 4568 §9.2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MkiSpec {
    /// Numeric MKI value.
    pub value: u64,
    /// MKI length in bytes (1–128 per RFC 4568).
    pub length: u8,
}

/// One `inline:` key-param from an `a=crypto:` line.
///
/// A single line MAY carry multiple semicolon-separated key-params; only
/// the first is typically used in SDES offer/answer, but parsing preserves
/// all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CryptoInlineKey {
    /// SRTP master key (suite-specific length).
    pub master_key: Vec<u8>,
    /// SRTP master salt (suite-specific length).
    pub master_salt: Vec<u8>,
    /// Optional lifetime token, preserved verbatim (`"2^31"`, `"1048576"`, …).
    /// Not interpreted — implementations re-key well before the default cap.
    pub lifetime: Option<String>,
    /// Optional MKI clause.
    pub mki: Option<MkiSpec>,
}

/// FEC ordering per RFC 4568 §6.3.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FecOrder {
    /// `FEC_ORDER=FEC_SRTP` — FEC applied before SRTP.
    FecSrtp,
    /// `FEC_ORDER=SRTP_FEC` — SRTP applied before FEC.
    SrtpFec,
}

/// Session params (RFC 4568 §6.3) that follow the key-params on an
/// `a=crypto:` line. Parsed into structured fields for the well-known
/// tokens; unknown params are preserved verbatim in `other` so the answer
/// can echo them back when policy allows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionParams {
    /// `UNENCRYPTED_SRTP` — RTP encryption is OFF for this session.
    pub unencrypted_srtp: bool,
    /// `UNENCRYPTED_SRTCP` — RTCP encryption is OFF.
    pub unencrypted_srtcp: bool,
    /// `UNAUTHENTICATED_SRTP` — RTP authentication is OFF.
    pub unauthenticated_srtp: bool,
    /// `KDR=<n>` where the key-derivation-rate is `2^n` packets.
    pub kdr: Option<u8>,
    /// `FEC_ORDER=…`.
    pub fec_order: Option<FecOrder>,
    /// `WSH=<n>` — replay window-size hint.
    pub wsh: Option<u64>,
    /// Tokens we don't interpret — preserved for round-trip.
    pub other: Vec<String>,
}

/// One fully parsed `a=crypto:` attribute.
///
/// Parse with [`CryptoAttribute::from_str`] (accepts both `"a=crypto:1 …"`
/// and the value-only `"1 …"` form) and emit with
/// [`CryptoAttribute::to_attribute_line`] or
/// [`CryptoAttribute::to_attribute_value`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CryptoAttribute {
    /// SDES tag — used by the answerer to point at the chosen offer.
    pub tag: u32,
    /// Negotiated crypto suite.
    pub suite: CryptoSuite,
    /// One or more inline keys (the first is the primary).
    pub keys: Vec<CryptoInlineKey>,
    /// Session params (often empty).
    pub session_params: SessionParams,
}

impl CryptoAttribute {
    /// Mint a fresh local attribute with random master key + master salt of
    /// the lengths required by `suite`. The lifetime and MKI clauses are
    /// omitted (RFC 4568 defaults apply).
    pub fn generate(tag: u32, suite: CryptoSuite) -> Self {
        let profile = suite.to_srtp_profile();
        let mut master_key = vec![0u8; profile.master_key_len()];
        let mut master_salt = vec![0u8; profile.master_salt_len()];
        rand::rng().fill_bytes(&mut master_key);
        rand::rng().fill_bytes(&mut master_salt);

        Self {
            tag,
            suite,
            keys: vec![CryptoInlineKey {
                master_key,
                master_salt,
                lifetime: None,
                mki: None,
            }],
            session_params: SessionParams::default(),
        }
    }

    /// Convert the primary inline key into `SrtpKeyMaterial`, ready to feed
    /// into `forge_rtp::SrtpContext`. Errors if there are no keys.
    pub fn to_srtp_key_material(&self) -> Result<SrtpKeyMaterial, SdesError> {
        let primary = self
            .keys
            .first()
            .ok_or_else(|| SdesError::Malformed("no inline key-params present".into()))?;
        SrtpKeyMaterial::new(
            primary.master_key.clone(),
            primary.master_salt.clone(),
            self.suite.to_srtp_profile(),
        )
        .map_err(|e| SdesError::Malformed(format!("key material rejected: {e}")))
    }

    /// Emit the full `a=crypto:` line (prefix + value), ready to push into SDP.
    pub fn to_attribute_line(&self) -> String {
        format!("a=crypto:{}", self.to_attribute_value())
    }

    /// Emit just the attribute *value* (no `a=crypto:` prefix). This is what
    /// `sip_sdp::Attribute::Value { name: "crypto", value }` expects.
    pub fn to_attribute_value(&self) -> String {
        let mut out = String::new();
        out.push_str(&self.tag.to_string());
        out.push(' ');
        out.push_str(self.suite.as_str());

        let mut first = true;
        for key in &self.keys {
            if first {
                out.push(' ');
                first = false;
            } else {
                out.push(';');
            }
            out.push_str("inline:");
            let mut blob = Vec::with_capacity(key.master_key.len() + key.master_salt.len());
            blob.extend_from_slice(&key.master_key);
            blob.extend_from_slice(&key.master_salt);
            out.push_str(&STANDARD.encode(&blob));
            if let Some(lifetime) = &key.lifetime {
                out.push('|');
                out.push_str(lifetime);
            }
            if let Some(mki) = &key.mki {
                out.push('|');
                out.push_str(&mki.value.to_string());
                out.push(':');
                out.push_str(&mki.length.to_string());
            }
        }

        emit_session_params(&self.session_params, &mut out);
        out
    }
}

impl FromStr for CryptoAttribute {
    type Err = SdesError;

    fn from_str(line: &str) -> Result<Self, Self::Err> {
        let trimmed = line.trim();
        // Accept both the full "a=crypto:…" form and the value-only form.
        let value = trimmed.strip_prefix("a=crypto:").unwrap_or(trimmed).trim();

        // RFC 4568 §9.2: 1*WSP between tokens. Split on whitespace.
        let mut tokens = value.split_whitespace();
        let tag_tok = tokens
            .next()
            .ok_or_else(|| SdesError::Malformed("missing tag".into()))?;
        let suite_tok = tokens
            .next()
            .ok_or_else(|| SdesError::Malformed("missing crypto-suite".into()))?;
        let key_tok = tokens
            .next()
            .ok_or_else(|| SdesError::Malformed("missing key-params".into()))?;

        let tag = parse_tag(tag_tok)?;
        let suite = suite_tok.parse::<CryptoSuite>()?;
        let keys = parse_key_params(key_tok, suite)?;

        let mut session_params = SessionParams::default();
        for tok in tokens {
            parse_session_param(tok, &mut session_params);
        }

        Ok(Self {
            tag,
            suite,
            keys,
            session_params,
        })
    }
}

fn parse_tag(s: &str) -> Result<u32, SdesError> {
    // RFC 4568 §9.2: tag = 1*9DIGIT
    if s.is_empty() || s.len() > 9 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(SdesError::InvalidTag(s.to_string()));
    }
    s.parse::<u32>()
        .map_err(|_| SdesError::InvalidTag(s.to_string()))
}

fn parse_key_params(key_tok: &str, suite: CryptoSuite) -> Result<Vec<CryptoInlineKey>, SdesError> {
    let mut out = Vec::new();
    for part in key_tok.split(';') {
        out.push(parse_single_key_param(part, suite)?);
    }
    if out.is_empty() {
        return Err(SdesError::Malformed("no key-params".into()));
    }
    Ok(out)
}

fn parse_single_key_param(part: &str, suite: CryptoSuite) -> Result<CryptoInlineKey, SdesError> {
    let body = part
        .strip_prefix("inline:")
        .ok_or_else(|| SdesError::UnsupportedKeyMethod(part.to_string()))?;

    // body = key-salt ["|" lifetime] ["|" mki]
    // The MKI clause contains a ':' so we can't split on '|' once and assume.
    let mut sections = body.split('|');
    let key_b64 = sections
        .next()
        .ok_or_else(|| SdesError::Malformed("missing key-salt".into()))?;

    let combined = STANDARD
        .decode(key_b64.as_bytes())
        .map_err(|e| SdesError::InvalidBase64(e.to_string()))?;

    let want = suite.key_info_len();
    if combined.len() != want {
        return Err(SdesError::KeyLengthMismatch {
            suite: suite.as_str(),
            expected: want,
            actual: combined.len(),
        });
    }

    let split = suite.to_srtp_profile().master_key_len();
    let master_key = combined[..split].to_vec();
    let master_salt = combined[split..].to_vec();

    let mut lifetime = None;
    let mut mki = None;
    for section in sections {
        if section.contains(':') {
            mki = Some(parse_mki(section)?);
        } else if lifetime.is_some() {
            return Err(SdesError::Malformed(format!(
                "unexpected extra key-param section: {section}"
            )));
        } else {
            lifetime = Some(section.to_string());
        }
    }

    Ok(CryptoInlineKey {
        master_key,
        master_salt,
        lifetime,
        mki,
    })
}

fn parse_mki(s: &str) -> Result<MkiSpec, SdesError> {
    let (value, length) = s
        .split_once(':')
        .ok_or_else(|| SdesError::InvalidMki(s.to_string()))?;
    let value = value
        .parse::<u64>()
        .map_err(|_| SdesError::InvalidMki(s.to_string()))?;
    let length = length
        .parse::<u8>()
        .map_err(|_| SdesError::InvalidMki(s.to_string()))?;
    if length == 0 || length > 128 {
        return Err(SdesError::InvalidMki(format!(
            "mki-length {length} out of range"
        )));
    }
    Ok(MkiSpec { value, length })
}

fn parse_session_param(tok: &str, params: &mut SessionParams) {
    match tok {
        "UNENCRYPTED_SRTP" => params.unencrypted_srtp = true,
        "UNENCRYPTED_SRTCP" => params.unencrypted_srtcp = true,
        "UNAUTHENTICATED_SRTP" => params.unauthenticated_srtp = true,
        _ => {
            if let Some(rest) = tok.strip_prefix("KDR=") {
                if let Ok(v) = rest.parse::<u8>() {
                    params.kdr = Some(v);
                    return;
                }
            } else if let Some(rest) = tok.strip_prefix("FEC_ORDER=") {
                let order = match rest {
                    "FEC_SRTP" => Some(FecOrder::FecSrtp),
                    "SRTP_FEC" => Some(FecOrder::SrtpFec),
                    _ => None,
                };
                if let Some(o) = order {
                    params.fec_order = Some(o);
                    return;
                }
            } else if let Some(rest) = tok.strip_prefix("WSH=") {
                if let Ok(v) = rest.parse::<u64>() {
                    params.wsh = Some(v);
                    return;
                }
            }
            params.other.push(tok.to_string());
        }
    }
}

fn emit_session_params(params: &SessionParams, out: &mut String) {
    if params.unencrypted_srtp {
        out.push_str(" UNENCRYPTED_SRTP");
    }
    if params.unencrypted_srtcp {
        out.push_str(" UNENCRYPTED_SRTCP");
    }
    if params.unauthenticated_srtp {
        out.push_str(" UNAUTHENTICATED_SRTP");
    }
    if let Some(kdr) = params.kdr {
        out.push_str(" KDR=");
        out.push_str(&kdr.to_string());
    }
    if let Some(order) = params.fec_order {
        out.push_str(" FEC_ORDER=");
        out.push_str(match order {
            FecOrder::FecSrtp => "FEC_SRTP",
            FecOrder::SrtpFec => "SRTP_FEC",
        });
    }
    if let Some(wsh) = params.wsh {
        out.push_str(" WSH=");
        out.push_str(&wsh.to_string());
    }
    for extra in &params.other {
        out.push(' ');
        out.push_str(extra);
    }
}

/// Pick the strongest mutually-supported crypto attribute from an offer.
///
/// `local_pref` is in preference order with the strongest suite first.
/// Returns a reference into `offered` so the caller can preserve the
/// offerer's tag in the answer (RFC 4568 §5.1.2).
pub fn select_crypto<'a>(
    offered: &'a [CryptoAttribute],
    local_pref: &[CryptoSuite],
) -> Option<&'a CryptoAttribute> {
    for &want in local_pref {
        if let Some(found) = offered.iter().find(|o| o.suite == want) {
            return Some(found);
        }
    }
    None
}

/// The answer side of an SDES offer/answer exchange.
#[derive(Debug, Clone)]
pub struct SdesAnswer {
    /// The `a=crypto:` attribute to put in the answer SDP.
    pub local_attribute: CryptoAttribute,
    /// SRTP key material to feed `forge_rtp::SrtpContext::set_remote_key`
    /// for decrypting inbound RTP (derived from the offerer's key).
    pub recv_key: SrtpKeyMaterial,
    /// SRTP key material to feed `forge_rtp::SrtpContext::set_local_key`
    /// for encrypting outbound RTP (derived from the locally minted key).
    pub send_key: SrtpKeyMaterial,
}

/// Mint the answer side of an SDES exchange.
///
/// Generates a fresh local master key + salt for `offer.suite`, preserves
/// `offer.tag` per RFC 4568 §5.1.2, and returns both the answer-side
/// attribute and the two key-material structs ready to wire into an SRTP
/// context.
pub fn answer_sdes(offer: &CryptoAttribute) -> Result<SdesAnswer, SdesError> {
    let local_attribute = CryptoAttribute::generate(offer.tag, offer.suite);
    let recv_key = offer.to_srtp_key_material()?;
    let send_key = local_attribute.to_srtp_key_material()?;
    Ok(SdesAnswer {
        local_attribute,
        recv_key,
        send_key,
    })
}

/// Session-level extension trait: read/write `a=crypto:` on `SessionDescription`.
///
/// Most SDP carriers put `a=crypto:` at the **media** level (see
/// [`MediaSdesAttributesExt`]); this trait covers the rare session-level
/// case for completeness.
pub trait SdesAttributesExt {
    /// Parse every well-formed `a=crypto:` attribute. Malformed lines are
    /// silently skipped — use [`Self::try_get_crypto_attributes`] to surface them.
    fn get_crypto_attributes(&self) -> Vec<CryptoAttribute>;

    /// Parse every `a=crypto:` attribute, preserving per-line errors.
    fn try_get_crypto_attributes(&self) -> Vec<Result<CryptoAttribute, SdesError>>;

    /// Append a fully-formed crypto attribute.
    fn add_crypto(&mut self, attr: &CryptoAttribute);

    /// Strip every `a=crypto:` attribute. Useful when downgrading to RTP/AVP.
    fn clear_crypto(&mut self);
}

/// Media-level extension trait: read/write `a=crypto:` on `MediaDescription`.
/// This is the form 99% of SDES traffic uses.
pub trait MediaSdesAttributesExt {
    /// See [`SdesAttributesExt::get_crypto_attributes`].
    fn get_media_crypto_attributes(&self) -> Vec<CryptoAttribute>;

    /// See [`SdesAttributesExt::try_get_crypto_attributes`].
    fn try_get_media_crypto_attributes(&self) -> Vec<Result<CryptoAttribute, SdesError>>;

    /// See [`SdesAttributesExt::add_crypto`].
    fn add_media_crypto(&mut self, attr: &CryptoAttribute);

    /// See [`SdesAttributesExt::clear_crypto`].
    fn clear_media_crypto(&mut self);
}

fn collect_crypto_results(attrs: &[Attribute]) -> Vec<Result<CryptoAttribute, SdesError>> {
    let mut out = Vec::new();
    for attr in attrs {
        if let Attribute::Value { name, value } = attr {
            if name == "crypto" {
                out.push(value.parse::<CryptoAttribute>());
            }
        }
    }
    out
}

fn append_crypto(attrs: &mut Vec<Attribute>, c: &CryptoAttribute) {
    attrs.push(Attribute::Value {
        name: SmolStr::new("crypto"),
        value: SmolStr::new(c.to_attribute_value()),
    });
}

fn strip_crypto(attrs: &mut Vec<Attribute>) {
    attrs.retain(|a| !matches!(a, Attribute::Value { name, .. } if name == "crypto"));
}

impl SdesAttributesExt for SessionDescription {
    fn get_crypto_attributes(&self) -> Vec<CryptoAttribute> {
        self.try_get_crypto_attributes()
            .into_iter()
            .filter_map(Result::ok)
            .collect()
    }

    fn try_get_crypto_attributes(&self) -> Vec<Result<CryptoAttribute, SdesError>> {
        collect_crypto_results(&self.attributes)
    }

    fn add_crypto(&mut self, attr: &CryptoAttribute) {
        append_crypto(&mut self.attributes, attr);
    }

    fn clear_crypto(&mut self) {
        strip_crypto(&mut self.attributes);
    }
}

impl MediaSdesAttributesExt for MediaDescription {
    fn get_media_crypto_attributes(&self) -> Vec<CryptoAttribute> {
        self.try_get_media_crypto_attributes()
            .into_iter()
            .filter_map(Result::ok)
            .collect()
    }

    fn try_get_media_crypto_attributes(&self) -> Vec<Result<CryptoAttribute, SdesError>> {
        collect_crypto_results(&self.attributes)
    }

    fn add_media_crypto(&mut self, attr: &CryptoAttribute) {
        append_crypto(&mut self.attributes, attr);
    }

    fn clear_media_crypto(&mut self) {
        strip_crypto(&mut self.attributes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------
    // CryptoSuite
    // ---------------------------------------------------------------------

    #[test]
    fn suite_round_trip() {
        for s in [
            CryptoSuite::Aes128CmHmacSha1_80,
            CryptoSuite::Aes128CmHmacSha1_32,
            CryptoSuite::AeadAes128Gcm,
            CryptoSuite::AeadAes256Gcm,
        ] {
            let token = s.as_str();
            assert_eq!(token.parse::<CryptoSuite>().unwrap(), s);
        }
    }

    #[test]
    fn suite_rejects_unknown() {
        let err = "AES_CM_256_HMAC_SHA1_80"
            .parse::<CryptoSuite>()
            .unwrap_err();
        assert!(matches!(err, SdesError::UnsupportedSuite(_)));
    }

    #[test]
    fn suite_key_info_lengths_match_rfcs() {
        // RFC 4568 §6.2: AES_CM_128_HMAC_SHA1_* → 30 bytes
        assert_eq!(CryptoSuite::Aes128CmHmacSha1_80.key_info_len(), 30);
        assert_eq!(CryptoSuite::Aes128CmHmacSha1_32.key_info_len(), 30);
        // RFC 7714: AEAD_AES_128_GCM → 28 bytes, AEAD_AES_256_GCM → 44 bytes
        assert_eq!(CryptoSuite::AeadAes128Gcm.key_info_len(), 28);
        assert_eq!(CryptoSuite::AeadAes256Gcm.key_info_len(), 44);
    }

    // ---------------------------------------------------------------------
    // Parsing — accept paths
    // ---------------------------------------------------------------------

    /// 30-byte payload base64-encoded — matches AES_CM_128_HMAC_SHA1_*.
    /// Concretely: 16 bytes of 0xAA (master key) + 14 bytes of 0x55 (master salt).
    /// The constant is checked against the round-trip in `key_30b_decodes_to_30_bytes`.
    const KEY_30B: &str = "qqqqqqqqqqqqqqqqqqqqqlVVVVVVVVVVVVVVVVVV";

    #[test]
    fn key_30b_decodes_to_30_bytes() {
        assert_eq!(STANDARD.decode(KEY_30B).unwrap().len(), 30);
    }

    #[test]
    fn parse_minimal_attribute_with_prefix() {
        let line = format!("a=crypto:1 AES_CM_128_HMAC_SHA1_80 inline:{KEY_30B}");
        let attr: CryptoAttribute = line.parse().unwrap();
        assert_eq!(attr.tag, 1);
        assert_eq!(attr.suite, CryptoSuite::Aes128CmHmacSha1_80);
        assert_eq!(attr.keys.len(), 1);
        assert_eq!(attr.keys[0].master_key.len(), 16);
        assert_eq!(attr.keys[0].master_salt.len(), 14);
        assert!(attr.keys[0].lifetime.is_none());
        assert!(attr.keys[0].mki.is_none());
        assert_eq!(attr.session_params, SessionParams::default());
    }

    #[test]
    fn parse_value_only_form() {
        let v = format!("2 AES_CM_128_HMAC_SHA1_32 inline:{KEY_30B}");
        let attr: CryptoAttribute = v.parse().unwrap();
        assert_eq!(attr.tag, 2);
        assert_eq!(attr.suite, CryptoSuite::Aes128CmHmacSha1_32);
    }

    #[test]
    fn parse_lifetime_and_mki() {
        let v = format!("7 AES_CM_128_HMAC_SHA1_80 inline:{KEY_30B}|2^31|1:4");
        let attr: CryptoAttribute = v.parse().unwrap();
        assert_eq!(attr.keys[0].lifetime.as_deref(), Some("2^31"));
        assert_eq!(
            attr.keys[0].mki,
            Some(MkiSpec {
                value: 1,
                length: 4
            })
        );
    }

    #[test]
    fn parse_mki_without_lifetime() {
        // RFC 4568 §9.2 allows omitting lifetime: inline:<key>|<mki>:<len>
        let v = format!("3 AES_CM_128_HMAC_SHA1_80 inline:{KEY_30B}|9:8");
        let attr: CryptoAttribute = v.parse().unwrap();
        assert!(attr.keys[0].lifetime.is_none());
        assert_eq!(
            attr.keys[0].mki,
            Some(MkiSpec {
                value: 9,
                length: 8
            })
        );
    }

    #[test]
    fn parse_multiple_keys_semicolon_separated() {
        let v = format!("5 AES_CM_128_HMAC_SHA1_80 inline:{KEY_30B};inline:{KEY_30B}|2^20");
        let attr: CryptoAttribute = v.parse().unwrap();
        assert_eq!(attr.keys.len(), 2);
        assert_eq!(attr.keys[1].lifetime.as_deref(), Some("2^20"));
    }

    #[test]
    fn parse_session_params() {
        let v = format!(
            "1 AES_CM_128_HMAC_SHA1_80 inline:{KEY_30B} \
             UNENCRYPTED_SRTCP KDR=4 FEC_ORDER=FEC_SRTP WSH=64 X-VENDOR=foo"
        );
        let attr: CryptoAttribute = v.parse().unwrap();
        assert!(!attr.session_params.unencrypted_srtp);
        assert!(attr.session_params.unencrypted_srtcp);
        assert!(!attr.session_params.unauthenticated_srtp);
        assert_eq!(attr.session_params.kdr, Some(4));
        assert_eq!(attr.session_params.fec_order, Some(FecOrder::FecSrtp));
        assert_eq!(attr.session_params.wsh, Some(64));
        assert_eq!(attr.session_params.other, vec!["X-VENDOR=foo".to_string()]);
    }

    #[test]
    fn parse_gcm_128() {
        // 28-byte payload (16 key + 12 salt) base64-encoded.
        let payload = STANDARD.encode([0xAAu8; 28]);
        let v = format!("1 AEAD_AES_128_GCM inline:{payload}");
        let attr: CryptoAttribute = v.parse().unwrap();
        assert_eq!(attr.keys[0].master_key.len(), 16);
        assert_eq!(attr.keys[0].master_salt.len(), 12);
    }

    #[test]
    fn parse_gcm_256() {
        // 44-byte payload (32 key + 12 salt) base64-encoded.
        let payload = STANDARD.encode([0xBBu8; 44]);
        let v = format!("1 AEAD_AES_256_GCM inline:{payload}");
        let attr: CryptoAttribute = v.parse().unwrap();
        assert_eq!(attr.keys[0].master_key.len(), 32);
        assert_eq!(attr.keys[0].master_salt.len(), 12);
    }

    // ---------------------------------------------------------------------
    // Parsing — reject paths
    // ---------------------------------------------------------------------

    #[test]
    fn rejects_missing_inline_prefix() {
        let v = format!("1 AES_CM_128_HMAC_SHA1_80 {KEY_30B}");
        assert!(matches!(
            v.parse::<CryptoAttribute>(),
            Err(SdesError::UnsupportedKeyMethod(_))
        ));
    }

    #[test]
    fn rejects_short_key_for_suite() {
        // 28-byte payload claimed under a 30-byte suite.
        let short = STANDARD.encode([0u8; 28]);
        let v = format!("1 AES_CM_128_HMAC_SHA1_80 inline:{short}");
        match v.parse::<CryptoAttribute>() {
            Err(SdesError::KeyLengthMismatch {
                expected, actual, ..
            }) => {
                assert_eq!(expected, 30);
                assert_eq!(actual, 28);
            }
            other => panic!("expected KeyLengthMismatch, got {other:?}"),
        }
    }

    #[test]
    fn rejects_long_key_for_suite() {
        // 44-byte payload claimed under a 30-byte suite — guards against
        // a peer that lies about the suite.
        let long = STANDARD.encode([0u8; 44]);
        let v = format!("1 AES_CM_128_HMAC_SHA1_80 inline:{long}");
        assert!(matches!(
            v.parse::<CryptoAttribute>(),
            Err(SdesError::KeyLengthMismatch { .. })
        ));
    }

    #[test]
    fn rejects_unknown_suite() {
        let v = format!("1 AES_CM_999_HMAC_SHA1_80 inline:{KEY_30B}");
        assert!(matches!(
            v.parse::<CryptoAttribute>(),
            Err(SdesError::UnsupportedSuite(_))
        ));
    }

    #[test]
    fn rejects_non_numeric_tag() {
        let v = format!("abc AES_CM_128_HMAC_SHA1_80 inline:{KEY_30B}");
        assert!(matches!(
            v.parse::<CryptoAttribute>(),
            Err(SdesError::InvalidTag(_))
        ));
    }

    #[test]
    fn rejects_tag_over_nine_digits() {
        let v = format!("1234567890 AES_CM_128_HMAC_SHA1_80 inline:{KEY_30B}");
        assert!(matches!(
            v.parse::<CryptoAttribute>(),
            Err(SdesError::InvalidTag(_))
        ));
    }

    #[test]
    fn rejects_invalid_base64() {
        let v = "1 AES_CM_128_HMAC_SHA1_80 inline:not%%base64";
        assert!(matches!(
            v.parse::<CryptoAttribute>(),
            Err(SdesError::InvalidBase64(_))
        ));
    }

    #[test]
    fn rejects_truncated() {
        assert!(matches!(
            "1".parse::<CryptoAttribute>(),
            Err(SdesError::Malformed(_))
        ));
        assert!(matches!(
            "1 AES_CM_128_HMAC_SHA1_80".parse::<CryptoAttribute>(),
            Err(SdesError::Malformed(_))
        ));
    }

    // ---------------------------------------------------------------------
    // Generate + round-trip
    // ---------------------------------------------------------------------

    #[test]
    fn generate_produces_valid_attribute() {
        let attr = CryptoAttribute::generate(1, CryptoSuite::Aes128CmHmacSha1_80);
        assert_eq!(attr.keys[0].master_key.len(), 16);
        assert_eq!(attr.keys[0].master_salt.len(), 14);
        assert!(attr
            .to_attribute_line()
            .starts_with("a=crypto:1 AES_CM_128_HMAC_SHA1_80 inline:"));
    }

    #[test]
    fn generate_keys_are_not_all_zero() {
        let attr = CryptoAttribute::generate(1, CryptoSuite::Aes128CmHmacSha1_80);
        assert!(attr.keys[0].master_key.iter().any(|&b| b != 0));
        assert!(attr.keys[0].master_salt.iter().any(|&b| b != 0));
    }

    #[test]
    fn round_trip_minimal() {
        let original = CryptoAttribute::generate(42, CryptoSuite::Aes128CmHmacSha1_80);
        let line = original.to_attribute_line();
        let reparsed: CryptoAttribute = line.parse().unwrap();
        assert_eq!(reparsed, original);
    }

    #[test]
    fn round_trip_with_session_params_and_mki() {
        let original = CryptoAttribute {
            tag: 9,
            suite: CryptoSuite::AeadAes128Gcm,
            keys: vec![CryptoInlineKey {
                master_key: vec![0xAA; 16],
                master_salt: vec![0xBB; 12],
                lifetime: Some("2^31".into()),
                mki: Some(MkiSpec {
                    value: 7,
                    length: 4,
                }),
            }],
            session_params: SessionParams {
                unencrypted_srtcp: true,
                kdr: Some(2),
                fec_order: Some(FecOrder::SrtpFec),
                wsh: Some(128),
                other: vec!["X-FOO=bar".into()],
                ..SessionParams::default()
            },
        };

        let line = original.to_attribute_line();
        let reparsed: CryptoAttribute = line.parse().unwrap();
        assert_eq!(reparsed, original);
    }

    // ---------------------------------------------------------------------
    // Key derivation hook into SrtpKeyMaterial
    // ---------------------------------------------------------------------

    #[test]
    fn to_srtp_key_material_succeeds() {
        let attr = CryptoAttribute::generate(1, CryptoSuite::Aes128CmHmacSha1_80);
        let km = attr.to_srtp_key_material().unwrap();
        assert_eq!(km.profile, SrtpProfile::Aes128CmHmacSha1_80);
        assert_eq!(km.master_key.len(), 16);
        assert_eq!(km.master_salt.len(), 14);
    }

    // ---------------------------------------------------------------------
    // Negotiation
    // ---------------------------------------------------------------------

    fn mk(tag: u32, suite: CryptoSuite) -> CryptoAttribute {
        CryptoAttribute::generate(tag, suite)
    }

    #[test]
    fn select_picks_strongest_first() {
        let offered = vec![
            mk(1, CryptoSuite::Aes128CmHmacSha1_32),
            mk(2, CryptoSuite::Aes128CmHmacSha1_80),
        ];
        let chosen = select_crypto(
            &offered,
            &[
                CryptoSuite::AeadAes128Gcm,
                CryptoSuite::Aes128CmHmacSha1_80,
                CryptoSuite::Aes128CmHmacSha1_32,
            ],
        )
        .unwrap();
        assert_eq!(chosen.suite, CryptoSuite::Aes128CmHmacSha1_80);
        assert_eq!(chosen.tag, 2);
    }

    #[test]
    fn select_returns_none_on_no_overlap() {
        let offered = vec![mk(1, CryptoSuite::Aes128CmHmacSha1_32)];
        let chosen = select_crypto(&offered, &[CryptoSuite::AeadAes256Gcm]);
        assert!(chosen.is_none());
    }

    #[test]
    fn answer_preserves_tag_and_suite_and_mints_fresh_keys() {
        let offer = mk(13, CryptoSuite::Aes128CmHmacSha1_80);
        let answer = answer_sdes(&offer).unwrap();
        assert_eq!(answer.local_attribute.tag, 13);
        assert_eq!(
            answer.local_attribute.suite,
            CryptoSuite::Aes128CmHmacSha1_80
        );
        // Local key must differ from the offered key (random with overwhelming
        // probability; 128 bits of entropy → collision practically impossible).
        assert_ne!(
            answer.local_attribute.keys[0].master_key,
            offer.keys[0].master_key
        );
        // recv_key matches offerer's key; send_key matches local minted key.
        assert_eq!(answer.recv_key.master_key, offer.keys[0].master_key);
        assert_eq!(
            answer.send_key.master_key,
            answer.local_attribute.keys[0].master_key
        );
    }

    // ---------------------------------------------------------------------
    // SessionDescription extension traits
    // ---------------------------------------------------------------------

    #[test]
    fn session_get_add_clear_crypto() {
        use sip_sdp::builder::SessionDescriptionBuilder;

        let mut sdp = SessionDescriptionBuilder::new()
            .origin("test", "1", "192.0.2.1")
            .unwrap()
            .session_name("t")
            .unwrap()
            .build();

        let a = CryptoAttribute::generate(1, CryptoSuite::Aes128CmHmacSha1_80);
        sdp.add_crypto(&a);
        sdp.add_crypto(&CryptoAttribute::generate(
            2,
            CryptoSuite::Aes128CmHmacSha1_32,
        ));

        let got = sdp.get_crypto_attributes();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], a);

        sdp.clear_crypto();
        assert!(sdp.get_crypto_attributes().is_empty());
    }

    #[test]
    fn media_get_add_clear_crypto() {
        let mut media = MediaDescription::audio(5004);
        media.add_media_crypto(&CryptoAttribute::generate(
            1,
            CryptoSuite::Aes128CmHmacSha1_80,
        ));
        assert_eq!(media.get_media_crypto_attributes().len(), 1);
        media.clear_media_crypto();
        assert!(media.get_media_crypto_attributes().is_empty());
    }

    #[test]
    fn try_get_surfaces_per_line_errors() {
        let mut media = MediaDescription::audio(5004);
        // Inject one good and one bad raw attribute.
        media.attributes.push(Attribute::Value {
            name: SmolStr::new("crypto"),
            value: SmolStr::new("garbage"),
        });
        media.add_media_crypto(&CryptoAttribute::generate(
            1,
            CryptoSuite::Aes128CmHmacSha1_80,
        ));

        let results = media.try_get_media_crypto_attributes();
        assert_eq!(results.len(), 2);
        assert!(results[0].is_err());
        assert!(results[1].is_ok());

        // The filtering accessor drops the broken one.
        assert_eq!(media.get_media_crypto_attributes().len(), 1);
    }
}
