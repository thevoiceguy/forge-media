//! RTCP packet handling

/// RTCP packet types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtcpPacketType {
    /// Sender Report
    SR = 200,
    /// Receiver Report
    RR = 201,
    /// Source Description
    SDES = 202,
    /// Goodbye
    BYE = 203,
    /// Application Defined
    APP = 204,
}

/// Placeholder for RTCP implementation
/// TODO: Implement full RTCP support
pub struct RtcpPacket;
