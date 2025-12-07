//! Forge SIP Server - Multi-User SIP Server with Forge RTP
//!
//! Supports user registration and call routing with Forge Media Engine integration.

use anyhow::{Context, Result};
use bytes::Bytes;
use dashmap::DashMap;
use forge_test_daemon::forge_client::{ForgeClient, SessionResponse};
use sip_core::{Method, Request, Response, SipUri};
use sip_parse::{parse_request, parse_response, serialize_request, serialize_response};
use sip_uas::UserAgentServer;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::UdpSocket;
use tracing::{debug, error, info, warn};

/// Registered user information
#[derive(Debug, Clone)]
struct Registration {
    username: String,
    contact_uri: String,
    remote_addr: SocketAddr,
    expires_at: u64,
}

/// Active call state
#[derive(Debug, Clone)]
struct CallState {
    call_id: String,
    forge_session: SessionResponse,
    caller: String,
    callee: String,
    caller_addr: SocketAddr,
    callee_addr: SocketAddr,
    state: CallStateEnum,
}

#[derive(Debug, Clone, PartialEq)]
enum CallStateEnum {
    Ringing,   // INVITE sent to callee, waiting for answer
    Answered,  // Callee sent 200 OK
    Active,    // ACK received, RTP flowing
}

/// Application state
struct AppState {
    uas: Arc<UserAgentServer>,
    forge_client: Arc<ForgeClient>,
    registrations: Arc<DashMap<String, Registration>>,
    calls: Arc<DashMap<String, CallState>>,
    socket: Arc<UdpSocket>,
    local_ip: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "forge_sip_server=info".into()),
        )
        .init();

    info!("🔨 Forge SIP Server - Multi-User Edition");
    info!("Supports registration and call routing with Forge RTP");
    info!("");

    let sip_bind: SocketAddr = "0.0.0.0:5060".parse().unwrap();
    let forge_api_url = "http://localhost:8081";
    let local_ip = "192.168.1.81";

    info!("Configuration:");
    info!("  SIP Listen: {}", sip_bind);
    info!("  Forge API: {}", forge_api_url);
    info!("  Domain: {}", local_ip);
    info!("");

    let forge_client = Arc::new(ForgeClient::new(forge_api_url));

    info!("Testing Forge API connection...");
    match forge_client.health_check().await {
        Ok(health) => {
            info!("✓ Forge API healthy (version {})", health.version);
        }
        Err(e) => {
            error!("✗ Failed to connect to Forge: {}", e);
            return Err(e.into());
        }
    }

    let socket = Arc::new(UdpSocket::bind(sip_bind).await?);
    info!("✓ SIP listening on {}", sip_bind);

    let local_uri = SipUri::parse(&format!("sip:forge@{}", local_ip))
        .context("Failed to parse local URI")?;
    let uas = Arc::new(UserAgentServer::new(local_uri.clone(), local_uri));

    let state = Arc::new(AppState {
        uas,
        forge_client,
        registrations: Arc::new(DashMap::new()),
        calls: Arc::new(DashMap::new()),
        socket,
        local_ip: local_ip.to_string(),
    });

    info!("");
    info!("✓ Server ready!");
    info!("");
    info!("Configure your softphones:");
    info!("  Domain: {}", local_ip);
    info!("  Port: 5060");
    info!("  Transport: UDP");
    info!("  Username: alice (or bob, charlie, etc.)");
    info!("  Password: <anything> (no auth)");
    info!("");
    info!("Then call between users: alice calls bob");
    info!("");

    let state_clone = state.clone();
    let processing_task = tokio::spawn(async move {
        let mut buf = vec![0u8; 65536];
        loop {
            match state_clone.socket.recv_from(&mut buf).await {
                Ok((len, from)) => {
                    let data = Bytes::copy_from_slice(&buf[..len]);
                    let state = state_clone.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_message(state, data, from).await {
                            error!("Error: {}", e);
                        }
                    });
                }
                Err(e) => error!("Socket error: {}", e),
            }
        }
    });

    // Periodic cleanup of expired registrations
    let state_clone = state.clone();
    let cleanup_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
            state_clone.registrations.retain(|_k, v| v.expires_at > now);
        }
    });

    tokio::signal::ctrl_c().await?;

    info!("");
    info!("Shutting down...");

    if !state.calls.is_empty() {
        info!("Cleaning up {} active calls", state.calls.len());
        for entry in state.calls.iter() {
            let _ = state.forge_client.delete_session(entry.key()).await;
        }
    }

    processing_task.abort();
    cleanup_task.abort();
    info!("✓ Server stopped");

    Ok(())
}

async fn handle_message(state: Arc<AppState>, msg: Bytes, from: SocketAddr) -> Result<()> {
    if let Some(req) = parse_request(&msg) {
        info!("← {:?} from {}", req.start.method, from);
        handle_request(state, req, from).await
    } else if let Some(resp) = parse_response(&msg) {
        info!("← {} response from {}", resp.start.code, from);
        handle_response(state, resp, from).await
    } else {
        warn!("Failed to parse from {}", from);
        Ok(())
    }
}

async fn handle_request(state: Arc<AppState>, req: Request, from: SocketAddr) -> Result<()> {
    match req.start.method {
        Method::Register => handle_register(state, req, from).await,
        Method::Invite => handle_invite(state, req, from).await,
        Method::Ack => handle_ack(state, req).await,
        Method::Bye => handle_bye(state, req, from).await,
        Method::Cancel => handle_cancel(state, req, from).await,
        Method::Options => handle_options(state, req, from).await,
        _ => {
            let resp = UserAgentServer::create_response(&req, 501, "Not Implemented");
            send_response(&state, resp, from).await
        }
    }
}

async fn handle_response(state: Arc<AppState>, resp: Response, _from: SocketAddr) -> Result<()> {
    // Extract Call-ID from response
    let call_id = resp.headers
        .get("Call-ID")
        .map(|v| v.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // Look up call state
    let call_state = match state.calls.get(&call_id) {
        Some(cs) => cs.clone(),
        None => {
            debug!("Response for unknown call: {}", call_id);
            return Ok(());
        }
    };

    info!("← {} from {} → forwarding to {}", resp.start.code, call_state.callee, call_state.caller);

    // For 200 OK responses, rewrite SDP and Contact header to point to Forge/server
    let mut modified_resp = resp.clone();
    if resp.start.code == 200 && !resp.body.is_empty() {
        // Parse and rewrite SDP
        let original_sdp = String::from_utf8_lossy(&resp.body);
        let rewritten_sdp = rewrite_sdp(
            &original_sdp,
            &state.local_ip,
            call_state.forge_session.rtp_port,
        );

        info!("  Rewriting SDP: IP={}, RTP port={}", state.local_ip, call_state.forge_session.rtp_port);

        // Update response body
        modified_resp.body = rewritten_sdp.into_bytes().into();

        // Update Content-Length header
        let body_len = modified_resp.body.len();
        modified_resp.headers.remove("Content-Length");
        modified_resp.headers.push("Content-Length".into(), body_len.to_string().into());

        // Rewrite Contact header to point to server
        // This ensures ACK is routed through us instead of directly to callee
        let server_contact = format!("<sip:{}@{}:5060>", call_state.callee, state.local_ip);
        info!("  Rewriting Contact: {}", server_contact);
        modified_resp.headers.remove("Contact");
        modified_resp.headers.push("Contact".into(), server_contact.into());
    }

    // Forward response to caller
    let resp_bytes = serialize_response(&modified_resp);
    state.socket.send_to(&resp_bytes, call_state.caller_addr).await?;

    // If 200 OK, mark call as Answered
    if resp.start.code == 200 {
        info!("✓ {} answered - waiting for ACK", call_state.callee);
        state.calls.alter(&call_id, |_, mut cs| {
            cs.state = CallStateEnum::Answered;
            cs
        });
    }

    Ok(())
}

async fn handle_register(state: Arc<AppState>, req: Request, from: SocketAddr) -> Result<()> {
    let username = extract_username(&req);
    let contact = extract_contact(&req);
    let expires = extract_expires(&req);

    info!("REGISTER from user '{}' (expires: {}s)", username, expires);

    if expires == 0 {
        // Unregister
        state.registrations.remove(&username);
        info!("✓ User '{}' unregistered", username);
    } else {
        // Register
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        state.registrations.insert(
            username.clone(),
            Registration {
                username: username.clone(),
                contact_uri: contact,
                remote_addr: from,
                expires_at: now + expires,
            },
        );
        info!("✓ User '{}' registered", username);
    }

    // Send 200 OK with Contact
    let mut resp = state.uas.create_ok(&req, None);
    if let Some(contact_hdr) = req.headers.get("Contact") {
        resp.headers.push("Contact".into(), contact_hdr.clone());
    }
    resp.headers.push("Expires".into(), expires.to_string().into());

    send_response(&state, resp, from).await?;

    // Show registered users
    info!("Registered users: {}", state.registrations.len());
    for entry in state.registrations.iter() {
        info!("  - {}", entry.key());
    }

    Ok(())
}

async fn handle_invite(state: Arc<AppState>, req: Request, from: SocketAddr) -> Result<()> {
    let call_id = extract_call_id(&req);
    let caller = extract_username(&req);
    let callee = extract_request_user(&req);

    info!("INVITE: {} → {} (call {})", caller, callee, call_id);

    // Send 100 Trying to caller
    let trying = UserAgentServer::create_trying(&req);
    send_response(&state, trying, from).await?;

    // Check if callee is registered
    let callee_reg = match state.registrations.get(&callee) {
        Some(reg) => reg.clone(),
        None => {
            warn!("User '{}' not registered", callee);
            let resp = UserAgentServer::create_response(&req, 404, "Not Found");
            send_response(&state, resp, from).await?;
            return Ok(());
        }
    };

    info!("Found callee '{}' at {}", callee, callee_reg.remote_addr);

    // Create Forge session
    info!("Creating Forge session...");
    let forge_session = match state.forge_client.create_session(&call_id).await {
        Ok(s) => {
            info!("✓ Forge session: RTP={} RTCP={}", s.rtp_port, s.rtcp_port);
            s
        }
        Err(e) => {
            error!("Failed to create Forge session: {}", e);
            let resp = UserAgentServer::create_response(&req, 500, "Server Error");
            send_response(&state, resp, from).await?;
            return Err(e.into());
        }
    };

    // Store call state as Ringing
    state.calls.insert(
        call_id.clone(),
        CallState {
            call_id: call_id.clone(),
            forge_session: forge_session.clone(),
            caller: caller.clone(),
            callee: callee.clone(),
            caller_addr: from,
            callee_addr: callee_reg.remote_addr,
            state: CallStateEnum::Ringing,
        },
    );

    // Create modified INVITE with Forge RTP in SDP to send to callee
    let sdp = create_sdp_answer(&state.local_ip, forge_session.rtp_port)?;
    let mut callee_invite = req.clone();
    callee_invite.body = sdp.into_bytes().into();

    // Update Content-Length
    let body_len = callee_invite.body.len();
    callee_invite.headers.remove("Content-Length");
    callee_invite.headers.push("Content-Length".into(), body_len.to_string().into());

    // Forward INVITE to callee
    info!("→ Forwarding INVITE to {} (RTP port {})", callee, forge_session.rtp_port);
    let invite_bytes = serialize_request(&callee_invite);
    state.socket.send_to(&invite_bytes, callee_reg.remote_addr).await?;
    info!("✓ {} phone should be ringing", callee);

    Ok(())
}

async fn handle_ack(state: Arc<AppState>, req: Request) -> Result<()> {
    let call_id = extract_call_id(&req);
    info!("ACK for call {}", call_id);

    // Look up call state
    let call_state = match state.calls.get(&call_id) {
        Some(cs) => cs.clone(),
        None => {
            warn!("ACK for unknown call: {}", call_id);
            return Ok(());
        }
    };

    // Forward ACK to callee
    info!("→ Forwarding ACK to {}", call_state.callee);
    let ack_bytes = serialize_request(&req);
    state.socket.send_to(&ack_bytes, call_state.callee_addr).await?;

    // Start Forge RTP forwarding
    info!("Starting RTP forwarding...");
    if let Err(e) = state.forge_client.start_session(&call_id).await {
        error!("Failed to start Forge session: {}", e);
    } else {
        info!("✓ RTP forwarding ACTIVE");
        info!("  Both {} and {} sending RTP to port {}", call_state.caller, call_state.callee, call_state.forge_session.rtp_port);

        // Mark call as Active
        state.calls.alter(&call_id, |_, mut cs| {
            cs.state = CallStateEnum::Active;
            cs
        });
        info!("✓ Call established - RTP should be flowing through Forge");
    }

    Ok(())
}

async fn handle_bye(state: Arc<AppState>, req: Request, from: SocketAddr) -> Result<()> {
    let call_id = extract_call_id(&req);
    info!("BYE for call {}", call_id);

    // Remove call
    state.calls.remove(&call_id);

    // Delete Forge session
    info!("Stopping Forge session...");
    if let Err(e) = state.forge_client.delete_session(&call_id).await {
        error!("Failed to delete Forge session: {}", e);
    } else {
        info!("✓ Forge session stopped");
    }

    // Send 200 OK
    let ok = state.uas.create_ok(&req, None);
    send_response(&state, ok, from).await?;

    info!("✓ Call terminated");
    Ok(())
}

async fn handle_cancel(state: Arc<AppState>, req: Request, from: SocketAddr) -> Result<()> {
    let call_id = extract_call_id(&req);
    info!("CANCEL for call {}", call_id);

    let ok = state.uas.create_ok(&req, None);
    send_response(&state, ok, from).await?;

    if let Some((_, call)) = state.calls.remove(&call_id) {
        let _ = state.forge_client.delete_session(&call.call_id).await;
    }

    Ok(())
}

async fn handle_options(state: Arc<AppState>, req: Request, from: SocketAddr) -> Result<()> {
    debug!("OPTIONS");
    let ok = state.uas.create_ok(&req, None);
    send_response(&state, ok, from).await
}

async fn send_response(state: &AppState, resp: Response, to: SocketAddr) -> Result<()> {
    let bytes = serialize_response(&resp);
    state.socket.send_to(&bytes, to).await?;
    Ok(())
}

fn create_sdp_answer(local_ip: &str, rtp_port: u16) -> Result<String> {
    Ok(format!(
        "v=0\r\n\
         o=forge {} 0 IN IP4 {}\r\n\
         s=Forge Call\r\n\
         c=IN IP4 {}\r\n\
         t=0 0\r\n\
         m=audio {} RTP/AVP 0 8 101\r\n\
         a=rtpmap:0 PCMU/8000\r\n\
         a=rtpmap:8 PCMA/8000\r\n\
         a=rtpmap:101 telephone-event/8000\r\n\
         a=sendrecv\r\n",
        chrono::Utc::now().timestamp(),
        local_ip,
        local_ip,
        rtp_port
    ))
}

/// Rewrite SDP to replace IP and port with Forge's
fn rewrite_sdp(sdp: &str, forge_ip: &str, forge_port: u16) -> String {
    let mut result = String::new();

    for line in sdp.lines() {
        if line.starts_with("c=IN IP4 ") {
            // Replace connection IP
            result.push_str(&format!("c=IN IP4 {}\r\n", forge_ip));
        } else if line.starts_with("m=audio ") {
            // Replace media port in m=audio line
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                result.push_str(&format!("m=audio {} {}", forge_port, parts[2..].join(" ")));
                result.push_str("\r\n");
            } else {
                result.push_str(line);
                result.push_str("\r\n");
            }
        } else {
            result.push_str(line);
            result.push_str("\r\n");
        }
    }

    result
}

fn extract_call_id(req: &Request) -> String {
    req.headers
        .get("Call-ID")
        .map(|v| v.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn extract_username(req: &Request) -> String {
    req.headers
        .get("From")
        .and_then(|v| {
            v.split("sip:")
                .nth(1)
                .and_then(|s| s.split('@').next())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "anonymous".to_string())
}

fn extract_request_user(req: &Request) -> String {
    // Extract username from Request-URI
    if let Some(sip_uri) = req.start.uri.as_sip() {
        sip_uri.user.as_ref().map(|u| u.to_string()).unwrap_or_else(|| "unknown".to_string())
    } else {
        "unknown".to_string()
    }
}

fn extract_contact(req: &Request) -> String {
    req.headers
        .get("Contact")
        .map(|v| v.to_string())
        .unwrap_or_else(|| "sip:unknown".to_string())
}

fn extract_expires(req: &Request) -> u64 {
    req.headers
        .get("Expires")
        .and_then(|v| v.parse().ok())
        .unwrap_or(3600)
}
