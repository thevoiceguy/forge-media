//! BFCP — the Binary Floor Control Protocol (RFC 8855) — as a conference
//! floor server needs it.
//!
//! A SIP room system that shares a screen asks for the *floor* with BFCP
//! before it sends content, and expects to be told when it has it, when
//! it was refused, and when a chair took it away. This crate is the
//! protocol half of that: the wire codec, a floor server that runs one
//! floor per conference and reports what it needs decided, and the two
//! transports' bookkeeping. It knows nothing of SIP, SDP or media; the
//! conference server that owns the content floor drives it.
//!
//! - [`message`]: the [`Message`] a datagram or stream frame carries —
//!   its [`Header`] and [`Attribute`]s — with `parse` and `to_bytes`
//!   that hold to RFC 8855 §5 and never panic on hostile input.
//! - [`server`]: the [`FloorServer`], a transport-agnostic state machine
//!   for one floor: it answers `Hello`, takes `FloorRequest`s as pending
//!   and reports them as [`Event`]s for the owner to grant, deny or
//!   revoke, handles `FloorRelease` and `FloorQuery`, keeps the floor's
//!   subscribers told with `FloorStatus`, and turns every mistake into
//!   the `Error` RFC 8855 §13 asks for.
//! - [`transport`]: [`TcpFramer`] cuts a byte stream into messages;
//!   [`UdpPeer`] keeps one client's RFC 8855 §8 transactions over UDP —
//!   the response flag, a cached response replayed for a retransmitted
//!   request, one outstanding server-initiated transaction at a time
//!   retransmitted on T1 until acknowledged or given up on.
//!
//! What it leaves out, on purpose: floor chairs (`ChairAction` draws
//! `Unauthorized Operation`; the conference's host is the chair, through
//! its own API), more than one floor per conference, TLS and DTLS (a
//! `Use TLS` error is never sent; secure the transport around it), and
//! BFCP-level fragmentation (a fragment is refused; every message this
//! server sends fits a datagram, and a client's request has no business
//! being larger).

pub mod message;
pub mod server;
pub mod transport;

pub use message::{
    Attribute, Echo, ErrorCode, Header, Message, ParseError, ParseErrorKind, Primitive, Priority,
    RequestStatus, COMMON_HEADER_LEN, VERSION_RELIABLE, VERSION_UNRELIABLE,
};
pub use server::{Event, FloorServer, Handled, Notification, Request, Transport};
pub use transport::{TcpFramer, Tick, UdpInbound, UdpPeer, MAX_RETRANSMITS, T1};
