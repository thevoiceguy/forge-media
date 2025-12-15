//! SIP Signaling for SIPREC (RFC 7866)
//!
//! This module implements the SIP signaling required for SIPREC:
//! - INVITE messages with recording metadata
//! - SDP for media streams
//! - Dialog management
//! - BYE messages for session termination

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tokio::sync::RwLock;
use dashmap::DashMap;
use uuid::Uuid;

/// SIP method types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SipMethod {
    Invite,
    Ack,
    Bye,
    Cancel,
    Register,
    Options,
}

impl SipMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            SipMethod::Invite => "INVITE",
            SipMethod::Ack => "ACK",
            SipMethod::Bye => "BYE",
            SipMethod::Cancel => "CANCEL",
            SipMethod::Register => "REGISTER",
            SipMethod::Options => "OPTIONS",
        }
    }
}

/// SIP dialog state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogState {
    /// Initial state, INVITE sent but no response yet
    Initial,
    /// Early dialog, received 1xx provisional response
    Early,
    /// Confirmed dialog, received 2xx final response
    Confirmed,
    /// Dialog terminated
    Terminated,
}

/// SIP Dialog representation
///
/// A dialog represents the peer-to-peer SIP relationship between two user agents
/// that persists for some time. Dialogs are identified by Call-ID, local tag, and remote tag.
#[derive(Debug, Clone)]
pub struct SipDialog {
    /// Unique dialog identifier (Call-ID)
    pub call_id: String,

    /// Local tag (from From header)
    pub local_tag: String,

    /// Remote tag (from To header in response)
    pub remote_tag: Option<String>,

    /// Current dialog state
    pub state: DialogState,

    /// Local URI
    pub local_uri: String,

    /// Remote URI
    pub remote_uri: String,

    /// Local contact address
    pub local_contact: SocketAddr,

    /// Remote target (contact from response)
    pub remote_target: Option<String>,

    /// Local CSeq number
    pub local_cseq: u32,

    /// Remote CSeq number
    pub remote_cseq: Option<u32>,

    /// Route set
    pub route_set: Vec<String>,
}

impl SipDialog {
    /// Create a new dialog from an INVITE request
    pub fn new(
        call_id: String,
        local_tag: String,
        local_uri: String,
        remote_uri: String,
        local_contact: SocketAddr,
        initial_cseq: u32,
    ) -> Self {
        Self {
            call_id,
            local_tag,
            remote_tag: None,
            state: DialogState::Initial,
            local_uri,
            remote_uri,
            local_contact,
            remote_target: None,
            local_cseq: initial_cseq,
            remote_cseq: None,
            route_set: Vec::new(),
        }
    }

    /// Update dialog from a 2xx response
    pub fn confirm(&mut self, remote_tag: String, remote_contact: Option<String>) {
        self.remote_tag = Some(remote_tag);
        if let Some(contact) = remote_contact {
            self.remote_target = Some(contact);
        }
        self.state = DialogState::Confirmed;
    }

    /// Mark dialog as terminated
    pub fn terminate(&mut self) {
        self.state = DialogState::Terminated;
    }

    /// Get next CSeq number for outgoing request
    pub fn next_cseq(&mut self) -> u32 {
        self.local_cseq += 1;
        self.local_cseq
    }

    /// Check if dialog is confirmed
    pub fn is_confirmed(&self) -> bool {
        self.state == DialogState::Confirmed
    }

    /// Create a BYE request for this dialog
    pub fn create_bye(&mut self) -> Result<SipRequest, String> {
        if !self.is_confirmed() {
            return Err("Cannot send BYE for unconfirmed dialog".to_string());
        }

        let cseq = self.next_cseq();
        Ok(SipRequest::new_bye(
            &self.local_uri,
            &self.remote_uri,
            &self.call_id,
            cseq,
            self.local_contact,
        ))
    }
}

/// SIP Dialog Manager
///
/// Manages multiple SIP dialogs for SIPREC sessions.
#[derive(Debug, Clone)]
pub struct DialogManager {
    /// Active dialogs keyed by Call-ID
    dialogs: Arc<DashMap<String, Arc<RwLock<SipDialog>>>>,
}

impl DialogManager {
    /// Create a new dialog manager
    pub fn new() -> Self {
        Self {
            dialogs: Arc::new(DashMap::new()),
        }
    }

    /// Create and store a new dialog
    pub async fn create_dialog(
        &self,
        call_id: String,
        local_tag: String,
        local_uri: String,
        remote_uri: String,
        local_contact: SocketAddr,
        initial_cseq: u32,
    ) -> Arc<RwLock<SipDialog>> {
        let dialog = Arc::new(RwLock::new(SipDialog::new(
            call_id.clone(),
            local_tag,
            local_uri,
            remote_uri,
            local_contact,
            initial_cseq,
        )));

        self.dialogs.insert(call_id, dialog.clone());
        dialog
    }

    /// Get a dialog by Call-ID
    pub fn get_dialog(&self, call_id: &str) -> Option<Arc<RwLock<SipDialog>>> {
        self.dialogs.get(call_id).map(|entry| entry.clone())
    }

    /// Remove a dialog
    pub fn remove_dialog(&self, call_id: &str) -> Option<Arc<RwLock<SipDialog>>> {
        self.dialogs.remove(call_id).map(|(_, dialog)| dialog)
    }

    /// Get count of active dialogs
    pub fn dialog_count(&self) -> usize {
        self.dialogs.len()
    }

    /// Confirm a dialog with 2xx response data
    pub async fn confirm_dialog(
        &self,
        call_id: &str,
        remote_tag: String,
        remote_contact: Option<String>,
    ) -> Result<(), String> {
        if let Some(dialog) = self.get_dialog(call_id) {
            let mut dialog = dialog.write().await;
            dialog.confirm(remote_tag, remote_contact);
            Ok(())
        } else {
            Err(format!("Dialog not found: {}", call_id))
        }
    }

    /// Terminate a dialog
    pub async fn terminate_dialog(&self, call_id: &str) -> Result<(), String> {
        if let Some(dialog) = self.remove_dialog(call_id) {
            let mut dialog = dialog.write().await;
            dialog.terminate();
            Ok(())
        } else {
            Err(format!("Dialog not found: {}", call_id))
        }
    }
}

impl Default for DialogManager {
    fn default() -> Self {
        Self::new()
    }
}

/// SIP request builder for SIPREC
///
/// Creates properly formatted SIP requests for recording sessions.
#[derive(Debug, Clone)]
pub struct SipRequest {
    method: SipMethod,
    request_uri: String,
    from: String,
    to: String,
    call_id: String,
    cseq: u32,
    contact: String,
    local_addr: SocketAddr,
    headers: Vec<(String, String)>,
    body: Option<String>,
}

impl SipRequest {
    /// Create a new SIPREC INVITE request
    ///
    /// # Arguments
    ///
    /// * `from_uri` - SIP URI of the recording client (SRC)
    /// * `to_uri` - SIP URI of the recording server (SRS)
    /// * `local_addr` - Local IP address and port
    pub fn new_siprec_invite(
        from_uri: impl Into<String>,
        to_uri: impl Into<String>,
        local_addr: SocketAddr,
    ) -> Self {
        let from = from_uri.into();
        let to = to_uri.into();
        let call_id = format!("{}@{}", Uuid::new_v4(), local_addr.ip());
        let contact = format!("sip:{}:{}", local_addr.ip(), local_addr.port());

        let mut request = Self {
            method: SipMethod::Invite,
            request_uri: to.clone(),
            from: from.clone(),
            to: to.clone(),
            call_id,
            cseq: 1,
            contact,
            local_addr,
            headers: Vec::new(),
            body: None,
        };

        // Add SIPREC-specific headers
        request.add_header("Accept", "application/sdp, application/rs-metadata+xml");
        request.add_header("Require", "siprec");
        request.add_header("Content-Type", "multipart/mixed");

        request
    }

    /// Create a BYE request for an existing dialog
    pub fn new_bye(
        from_uri: impl Into<String>,
        to_uri: impl Into<String>,
        call_id: impl Into<String>,
        cseq: u32,
        local_addr: SocketAddr,
    ) -> Self {
        let from = from_uri.into();
        let to = to_uri.into();
        let contact = format!("sip:{}:{}", local_addr.ip(), local_addr.port());

        Self {
            method: SipMethod::Bye,
            request_uri: to.clone(),
            from,
            to,
            call_id: call_id.into(),
            cseq,
            contact,
            local_addr,
            headers: Vec::new(),
            body: None,
        }
    }

    /// Add a custom SIP header
    pub fn add_header(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.headers.push((name.into(), value.into()));
    }

    /// Set the SDP body
    pub fn set_sdp_body(&mut self, sdp: impl Into<String>) {
        self.body = Some(sdp.into());
        // Update content type if not multipart
        if !self.headers.iter().any(|(k, v)| k == "Content-Type" && v.contains("multipart")) {
            self.headers.retain(|(k, _)| k != "Content-Type");
            self.add_header("Content-Type", "application/sdp");
        }
    }

    /// Set a multipart body with SDP and metadata
    pub fn set_multipart_body(&mut self, sdp: impl Into<String>, metadata_xml: impl Into<String>) {
        let boundary = format!("boundary-{}", Uuid::new_v4());

        let sdp = sdp.into();
        let metadata = metadata_xml.into();

        let body = format!(
            "--{boundary}\r\n\
             Content-Type: application/sdp\r\n\
             \r\n\
             {sdp}\r\n\
             --{boundary}\r\n\
             Content-Type: application/rs-metadata+xml\r\n\
             \r\n\
             {metadata}\r\n\
             --{boundary}--\r\n",
            boundary = boundary,
            sdp = sdp,
            metadata = metadata
        );

        self.body = Some(body);
        self.headers.retain(|(k, _)| k != "Content-Type");
        self.add_header("Content-Type", format!("multipart/mixed;boundary={}", boundary));
    }

    /// Build the SIP request as a string
    pub fn build(&self) -> String {
        let mut request = String::new();

        // Request line
        request.push_str(&format!(
            "{} {} SIP/2.0\r\n",
            self.method.as_str(),
            self.request_uri
        ));

        // Via header
        request.push_str(&format!(
            "Via: SIP/2.0/UDP {};branch=z9hG4bK{}\r\n",
            self.local_addr,
            Uuid::new_v4()
        ));

        // Max-Forwards
        request.push_str("Max-Forwards: 70\r\n");

        // From header
        request.push_str(&format!(
            "From: <{}>;tag={}\r\n",
            self.from,
            generate_tag()
        ));

        // To header
        request.push_str(&format!("To: <{}>\r\n", self.to));

        // Call-ID
        request.push_str(&format!("Call-ID: {}\r\n", self.call_id));

        // CSeq
        request.push_str(&format!("CSeq: {} {}\r\n", self.cseq, self.method.as_str()));

        // Contact
        request.push_str(&format!("Contact: <{}>\r\n", self.contact));

        // User-Agent
        request.push_str("User-Agent: Forge-SIPREC/1.0\r\n");

        // Custom headers
        for (name, value) in &self.headers {
            request.push_str(&format!("{}: {}\r\n", name, value));
        }

        // Content-Length
        let content_length = self.body.as_ref().map(|b| b.len()).unwrap_or(0);
        request.push_str(&format!("Content-Length: {}\r\n", content_length));

        // End of headers
        request.push_str("\r\n");

        // Body
        if let Some(body) = &self.body {
            request.push_str(body);
        }

        request
    }

    /// Get the Call-ID
    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    /// Get the CSeq number
    pub fn cseq(&self) -> u32 {
        self.cseq
    }
}

/// SDP (Session Description Protocol) builder for SIPREC
///
/// Creates SDP for recording sessions with RTP media streams.
#[derive(Debug, Clone)]
pub struct SdpBuilder {
    session_id: String,
    session_version: u64,
    origin_addr: IpAddr,
    session_name: String,
    connection_addr: IpAddr,
    media_streams: Vec<MediaDescription>,
}

/// Media stream description
#[derive(Debug, Clone)]
pub struct MediaDescription {
    media_type: String,  // "audio", "video", etc.
    port: u16,
    protocol: String,    // "RTP/AVP", "RTP/SAVP"
    formats: Vec<u8>,    // Payload types
    rtpmap: Vec<String>, // Format descriptions
    attributes: Vec<String>,
}

impl SdpBuilder {
    /// Create a new SDP builder
    pub fn new(origin_addr: IpAddr, connection_addr: IpAddr) -> Self {
        Self {
            session_id: rand::random::<u64>().to_string(),
            session_version: 1,
            origin_addr,
            session_name: "SIPREC Recording Session".to_string(),
            connection_addr,
            media_streams: Vec::new(),
        }
    }

    /// Add an audio stream
    pub fn add_audio_stream(
        &mut self,
        port: u16,
        codec: &str,
        payload_type: u8,
        sample_rate: u32,
    ) {
        let mut media = MediaDescription {
            media_type: "audio".to_string(),
            port,
            protocol: "RTP/AVP".to_string(),
            formats: vec![payload_type],
            rtpmap: vec![format!("{} {}/{}", payload_type, codec, sample_rate)],
            attributes: Vec::new(),
        };

        // Add common audio attributes
        media.attributes.push("sendonly".to_string());
        media.attributes.push("label:recording".to_string());

        self.media_streams.push(media);
    }

    /// Add SRTP (secure RTP) support
    pub fn enable_srtp(&mut self, stream_index: usize, crypto_suite: &str, key: &str) {
        if let Some(media) = self.media_streams.get_mut(stream_index) {
            media.protocol = "RTP/SAVP".to_string();
            media.attributes.push(format!("crypto:1 {} inline:{}", crypto_suite, key));
        }
    }

    /// Build the SDP as a string
    pub fn build(&self) -> String {
        let mut sdp = String::new();

        // Session description
        sdp.push_str("v=0\r\n");
        sdp.push_str(&format!(
            "o=- {} {} IN IP4 {}\r\n",
            self.session_id, self.session_version, self.origin_addr
        ));
        sdp.push_str(&format!("s={}\r\n", self.session_name));
        sdp.push_str(&format!("c=IN IP4 {}\r\n", self.connection_addr));
        sdp.push_str("t=0 0\r\n");

        // Media descriptions
        for media in &self.media_streams {
            sdp.push_str(&format!(
                "m={} {} {}",
                media.media_type,
                media.port,
                media.protocol
            ));
            for fmt in &media.formats {
                sdp.push_str(&format!(" {}", fmt));
            }
            sdp.push_str("\r\n");

            // RTP map
            for rtpmap in &media.rtpmap {
                sdp.push_str(&format!("a=rtpmap:{}\r\n", rtpmap));
            }

            // Attributes
            for attr in &media.attributes {
                sdp.push_str(&format!("a={}\r\n", attr));
            }
        }

        sdp
    }
}

/// Generate a random SIP tag
fn generate_tag() -> String {
    format!("{:x}", rand::random::<u64>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sip_invite_generation() {
        let local_addr: SocketAddr = "192.168.1.100:5060".parse().unwrap();
        let request = SipRequest::new_siprec_invite(
            "sip:src@example.com",
            "sip:srs@recorder.example.com",
            local_addr,
        );

        let sip_message = request.build();

        assert!(sip_message.starts_with("INVITE"));
        assert!(sip_message.contains("SIP/2.0"));
        assert!(sip_message.contains("Require: siprec"));
        assert!(sip_message.contains("application/rs-metadata+xml"));
    }

    #[test]
    fn test_sdp_generation() {
        let mut sdp = SdpBuilder::new(
            "192.168.1.100".parse().unwrap(),
            "192.168.1.100".parse().unwrap(),
        );

        sdp.add_audio_stream(5004, "PCMU", 0, 8000);

        let sdp_str = sdp.build();

        assert!(sdp_str.contains("v=0"));
        assert!(sdp_str.contains("m=audio 5004"));
        assert!(sdp_str.contains("a=rtpmap:0 PCMU/8000"));
        assert!(sdp_str.contains("a=sendonly"));
    }

    #[test]
    fn test_bye_generation() {
        let local_addr: SocketAddr = "192.168.1.100:5060".parse().unwrap();
        let request = SipRequest::new_bye(
            "sip:src@example.com",
            "sip:srs@recorder.example.com",
            "test-call-id",
            2,
            local_addr,
        );

        let sip_message = request.build();

        assert!(sip_message.starts_with("BYE"));
        assert!(sip_message.contains("CSeq: 2 BYE"));
        assert!(sip_message.contains("Call-ID: test-call-id"));
    }

    #[test]
    fn test_multipart_body() {
        let local_addr: SocketAddr = "192.168.1.100:5060".parse().unwrap();
        let mut request = SipRequest::new_siprec_invite(
            "sip:src@example.com",
            "sip:srs@recorder.example.com",
            local_addr,
        );

        let sdp = "v=0\r\no=- 123 1 IN IP4 192.168.1.100\r\n";
        let metadata = "<recording session=\"test\"></recording>";

        request.set_multipart_body(sdp, metadata);

        let sip_message = request.build();

        assert!(sip_message.contains("multipart/mixed"));
        assert!(sip_message.contains("application/sdp"));
        assert!(sip_message.contains("application/rs-metadata+xml"));
        assert!(sip_message.contains(sdp));
        assert!(sip_message.contains(metadata));
    }

    #[tokio::test]
    async fn test_dialog_creation() {
        let manager = DialogManager::new();
        let local_addr: SocketAddr = "192.168.1.100:5060".parse().unwrap();

        let dialog = manager
            .create_dialog(
                "call-123".to_string(),
                "tag-abc".to_string(),
                "sip:src@example.com".to_string(),
                "sip:srs@recorder.example.com".to_string(),
                local_addr,
                1,
            )
            .await;

        let dialog_read = dialog.read().await;
        assert_eq!(dialog_read.call_id, "call-123");
        assert_eq!(dialog_read.state, DialogState::Initial);
        assert_eq!(manager.dialog_count(), 1);
    }

    #[tokio::test]
    async fn test_dialog_confirmation() {
        let manager = DialogManager::new();
        let local_addr: SocketAddr = "192.168.1.100:5060".parse().unwrap();

        manager
            .create_dialog(
                "call-456".to_string(),
                "tag-local".to_string(),
                "sip:src@example.com".to_string(),
                "sip:srs@recorder.example.com".to_string(),
                local_addr,
                1,
            )
            .await;

        manager
            .confirm_dialog(
                "call-456",
                "tag-remote".to_string(),
                Some("sip:remote@192.168.1.200:5060".to_string()),
            )
            .await
            .unwrap();

        let dialog = manager.get_dialog("call-456").unwrap();
        let dialog_read = dialog.read().await;
        assert_eq!(dialog_read.state, DialogState::Confirmed);
        assert_eq!(dialog_read.remote_tag, Some("tag-remote".to_string()));
        assert!(dialog_read.is_confirmed());
    }

    #[tokio::test]
    async fn test_dialog_bye_creation() {
        let manager = DialogManager::new();
        let local_addr: SocketAddr = "192.168.1.100:5060".parse().unwrap();

        let dialog_arc = manager
            .create_dialog(
                "call-789".to_string(),
                "tag-local".to_string(),
                "sip:src@example.com".to_string(),
                "sip:srs@recorder.example.com".to_string(),
                local_addr,
                1,
            )
            .await;

        // Confirm the dialog first
        {
            let mut dialog = dialog_arc.write().await;
            dialog.confirm("tag-remote".to_string(), None);
        }

        // Create BYE request
        let bye_request = {
            let mut dialog = dialog_arc.write().await;
            dialog.create_bye().unwrap()
        };

        let bye_message = bye_request.build();
        assert!(bye_message.starts_with("BYE"));
        assert!(bye_message.contains("call-789"));
    }

    #[tokio::test]
    async fn test_dialog_termination() {
        let manager = DialogManager::new();
        let local_addr: SocketAddr = "192.168.1.100:5060".parse().unwrap();

        manager
            .create_dialog(
                "call-999".to_string(),
                "tag-abc".to_string(),
                "sip:src@example.com".to_string(),
                "sip:srs@recorder.example.com".to_string(),
                local_addr,
                1,
            )
            .await;

        assert_eq!(manager.dialog_count(), 1);

        manager.terminate_dialog("call-999").await.unwrap();

        assert_eq!(manager.dialog_count(), 0);
    }

    #[tokio::test]
    async fn test_dialog_cseq_increment() {
        let local_addr: SocketAddr = "192.168.1.100:5060".parse().unwrap();
        let mut dialog = SipDialog::new(
            "call-cseq".to_string(),
            "tag-local".to_string(),
            "sip:src@example.com".to_string(),
            "sip:srs@recorder.example.com".to_string(),
            local_addr,
            1,
        );

        assert_eq!(dialog.local_cseq, 1);
        let next = dialog.next_cseq();
        assert_eq!(next, 2);
        assert_eq!(dialog.local_cseq, 2);
    }
}
