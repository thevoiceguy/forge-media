//! BFCP messages on the wire (RFC 8855 §5).
//!
//! A message is a 12-octet [`Header`] followed by attributes, each a
//! type, a mandatory bit, a length and a value padded to four octets.
//! Five of the attributes are *grouped*: their value is a 16-bit id and
//! more attributes. [`Message::parse`] reads one from a datagram or a
//! stream frame and [`Message::to_bytes`] writes one; both are exact
//! about lengths, and the parser never panics whatever the bytes say.

use std::fmt;

/// The version a message carries over a reliable transport (TCP).
pub const VERSION_RELIABLE: u8 = 1;
/// The version a message carries over an unreliable transport (UDP).
pub const VERSION_UNRELIABLE: u8 = 2;
/// The common header's size in octets.
pub const COMMON_HEADER_LEN: usize = 12;
/// The fragment fields' size, present when the F flag is set.
const FRAGMENT_LEN: usize = 4;
/// The longest payload a header can describe: the length is in 4-octet
/// units.
const MAX_PAYLOAD_LEN: usize = u16::MAX as usize * 4;

/// The message types (RFC 8855 §5.1, Table 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Primitive {
    FloorRequest = 1,
    FloorRelease = 2,
    FloorRequestQuery = 3,
    FloorRequestStatus = 4,
    UserQuery = 5,
    UserStatus = 6,
    FloorQuery = 7,
    FloorStatus = 8,
    ChairAction = 9,
    ChairActionAck = 10,
    Hello = 11,
    HelloAck = 12,
    Error = 13,
    FloorRequestStatusAck = 14,
    FloorStatusAck = 15,
    Goodbye = 16,
    GoodbyeAck = 17,
}

impl Primitive {
    /// Every primitive, in wire order: what a `HelloAck` lists.
    pub const ALL: [Primitive; 17] = [
        Primitive::FloorRequest,
        Primitive::FloorRelease,
        Primitive::FloorRequestQuery,
        Primitive::FloorRequestStatus,
        Primitive::UserQuery,
        Primitive::UserStatus,
        Primitive::FloorQuery,
        Primitive::FloorStatus,
        Primitive::ChairAction,
        Primitive::ChairActionAck,
        Primitive::Hello,
        Primitive::HelloAck,
        Primitive::Error,
        Primitive::FloorRequestStatusAck,
        Primitive::FloorStatusAck,
        Primitive::Goodbye,
        Primitive::GoodbyeAck,
    ];

    pub fn from_u8(v: u8) -> Option<Primitive> {
        Primitive::ALL.iter().copied().find(|p| *p as u8 == v)
    }

    pub fn as_u8(self) -> u8 {
        self as u8
    }

    /// The RFC's name.
    pub fn name(self) -> &'static str {
        match self {
            Primitive::FloorRequest => "FloorRequest",
            Primitive::FloorRelease => "FloorRelease",
            Primitive::FloorRequestQuery => "FloorRequestQuery",
            Primitive::FloorRequestStatus => "FloorRequestStatus",
            Primitive::UserQuery => "UserQuery",
            Primitive::UserStatus => "UserStatus",
            Primitive::FloorQuery => "FloorQuery",
            Primitive::FloorStatus => "FloorStatus",
            Primitive::ChairAction => "ChairAction",
            Primitive::ChairActionAck => "ChairActionAck",
            Primitive::Hello => "Hello",
            Primitive::HelloAck => "HelloAck",
            Primitive::Error => "Error",
            Primitive::FloorRequestStatusAck => "FloorRequestStatusAck",
            Primitive::FloorStatusAck => "FloorStatusAck",
            Primitive::Goodbye => "Goodbye",
            Primitive::GoodbyeAck => "GoodbyeAck",
        }
    }

    /// Whether a client sends this to acknowledge a server-initiated
    /// transaction over an unreliable transport (RFC 8855 §8.2).
    pub fn is_ack(self) -> bool {
        matches!(
            self,
            Primitive::FloorRequestStatusAck | Primitive::FloorStatusAck | Primitive::GoodbyeAck
        )
    }
}

impl fmt::Display for Primitive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Where a floor request stands (RFC 8855 §5.2.5, Table 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum RequestStatus {
    Pending = 1,
    Accepted = 2,
    Granted = 3,
    Denied = 4,
    Cancelled = 5,
    Released = 6,
    Revoked = 7,
}

impl RequestStatus {
    pub fn from_u8(v: u8) -> Option<RequestStatus> {
        [
            RequestStatus::Pending,
            RequestStatus::Accepted,
            RequestStatus::Granted,
            RequestStatus::Denied,
            RequestStatus::Cancelled,
            RequestStatus::Released,
            RequestStatus::Revoked,
        ]
        .into_iter()
        .find(|s| *s as u8 == v)
    }

    pub fn name(self) -> &'static str {
        match self {
            RequestStatus::Pending => "Pending",
            RequestStatus::Accepted => "Accepted",
            RequestStatus::Granted => "Granted",
            RequestStatus::Denied => "Denied",
            RequestStatus::Cancelled => "Cancelled",
            RequestStatus::Released => "Released",
            RequestStatus::Revoked => "Revoked",
        }
    }

    /// Whether the request is over: the server may forget it (§13.1.2).
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            RequestStatus::Denied
                | RequestStatus::Cancelled
                | RequestStatus::Released
                | RequestStatus::Revoked
        )
    }
}

impl fmt::Display for RequestStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What an `Error` says went wrong (RFC 8855 §5.2.6, Table 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    ConferenceDoesNotExist,
    UserDoesNotExist,
    UnknownPrimitive,
    UnknownMandatoryAttribute,
    UnauthorizedOperation,
    InvalidFloorId,
    FloorRequestIdDoesNotExist,
    MaximumFloorRequestsReached,
    UseTls,
    UnableToParseMessage,
    UseDtls,
    UnsupportedVersion,
    IncorrectMessageLength,
    GenericError,
    /// A code this crate does not know.
    Other(u8),
}

impl ErrorCode {
    pub fn from_u8(v: u8) -> ErrorCode {
        match v {
            1 => ErrorCode::ConferenceDoesNotExist,
            2 => ErrorCode::UserDoesNotExist,
            3 => ErrorCode::UnknownPrimitive,
            4 => ErrorCode::UnknownMandatoryAttribute,
            5 => ErrorCode::UnauthorizedOperation,
            6 => ErrorCode::InvalidFloorId,
            7 => ErrorCode::FloorRequestIdDoesNotExist,
            8 => ErrorCode::MaximumFloorRequestsReached,
            9 => ErrorCode::UseTls,
            10 => ErrorCode::UnableToParseMessage,
            11 => ErrorCode::UseDtls,
            12 => ErrorCode::UnsupportedVersion,
            13 => ErrorCode::IncorrectMessageLength,
            14 => ErrorCode::GenericError,
            other => ErrorCode::Other(other),
        }
    }

    pub fn as_u8(self) -> u8 {
        match self {
            ErrorCode::ConferenceDoesNotExist => 1,
            ErrorCode::UserDoesNotExist => 2,
            ErrorCode::UnknownPrimitive => 3,
            ErrorCode::UnknownMandatoryAttribute => 4,
            ErrorCode::UnauthorizedOperation => 5,
            ErrorCode::InvalidFloorId => 6,
            ErrorCode::FloorRequestIdDoesNotExist => 7,
            ErrorCode::MaximumFloorRequestsReached => 8,
            ErrorCode::UseTls => 9,
            ErrorCode::UnableToParseMessage => 10,
            ErrorCode::UseDtls => 11,
            ErrorCode::UnsupportedVersion => 12,
            ErrorCode::IncorrectMessageLength => 13,
            ErrorCode::GenericError => 14,
            ErrorCode::Other(v) => v,
        }
    }

    /// The RFC's wording.
    pub fn name(self) -> &'static str {
        match self {
            ErrorCode::ConferenceDoesNotExist => "Conference Does Not Exist",
            ErrorCode::UserDoesNotExist => "User Does Not Exist",
            ErrorCode::UnknownPrimitive => "Unknown Primitive",
            ErrorCode::UnknownMandatoryAttribute => "Unknown Mandatory Attribute",
            ErrorCode::UnauthorizedOperation => "Unauthorized Operation",
            ErrorCode::InvalidFloorId => "Invalid Floor ID",
            ErrorCode::FloorRequestIdDoesNotExist => "Floor Request ID Does Not Exist",
            ErrorCode::MaximumFloorRequestsReached => {
                "You have Already Reached the Maximum Number of Ongoing Floor Requests for This Floor"
            }
            ErrorCode::UseTls => "Use TLS",
            ErrorCode::UnableToParseMessage => "Unable to Parse Message",
            ErrorCode::UseDtls => "Use DTLS",
            ErrorCode::UnsupportedVersion => "Unsupported Version",
            ErrorCode::IncorrectMessageLength => "Incorrect Message Length",
            ErrorCode::GenericError => "Generic Error",
            ErrorCode::Other(_) => "Unknown Error Code",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.name(), self.as_u8())
    }
}

/// A request's priority (RFC 8855 §5.2.4). Values above 4 read as
/// `Highest`, as the RFC says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[repr(u8)]
pub enum Priority {
    Lowest = 0,
    Low = 1,
    #[default]
    Normal = 2,
    High = 3,
    Highest = 4,
}

impl Priority {
    pub fn from_u8(v: u8) -> Priority {
        match v {
            0 => Priority::Lowest,
            1 => Priority::Low,
            2 => Priority::Normal,
            3 => Priority::High,
            _ => Priority::Highest,
        }
    }
}

/// The attribute types (RFC 8855 §5.2, Table 3).
pub mod attr_type {
    pub const BENEFICIARY_ID: u8 = 1;
    pub const FLOOR_ID: u8 = 2;
    pub const FLOOR_REQUEST_ID: u8 = 3;
    pub const PRIORITY: u8 = 4;
    pub const REQUEST_STATUS: u8 = 5;
    pub const ERROR_CODE: u8 = 6;
    pub const ERROR_INFO: u8 = 7;
    pub const PARTICIPANT_PROVIDED_INFO: u8 = 8;
    pub const STATUS_INFO: u8 = 9;
    pub const SUPPORTED_ATTRIBUTES: u8 = 10;
    pub const SUPPORTED_PRIMITIVES: u8 = 11;
    pub const USER_DISPLAY_NAME: u8 = 12;
    pub const USER_URI: u8 = 13;
    pub const BENEFICIARY_INFORMATION: u8 = 14;
    pub const FLOOR_REQUEST_INFORMATION: u8 = 15;
    pub const REQUESTED_BY_INFORMATION: u8 = 16;
    pub const FLOOR_REQUEST_STATUS: u8 = 17;
    pub const OVERALL_REQUEST_STATUS: u8 = 18;
    /// Every type this crate understands, in wire order: what a
    /// `HelloAck` lists.
    pub const ALL: [u8; 18] = [
        BENEFICIARY_ID,
        FLOOR_ID,
        FLOOR_REQUEST_ID,
        PRIORITY,
        REQUEST_STATUS,
        ERROR_CODE,
        ERROR_INFO,
        PARTICIPANT_PROVIDED_INFO,
        STATUS_INFO,
        SUPPORTED_ATTRIBUTES,
        SUPPORTED_PRIMITIVES,
        USER_DISPLAY_NAME,
        USER_URI,
        BENEFICIARY_INFORMATION,
        FLOOR_REQUEST_INFORMATION,
        REQUESTED_BY_INFORMATION,
        FLOOR_REQUEST_STATUS,
        OVERALL_REQUEST_STATUS,
    ];
}

/// One attribute (RFC 8855 §5.2). The grouped ones carry the 16-bit id
/// their header holds and the attributes inside them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attribute {
    BeneficiaryId(u16),
    FloorId(u16),
    FloorRequestId(u16),
    Priority(Priority),
    RequestStatus {
        status: RequestStatus,
        /// In the floor's queue; 0 when not applicable.
        queue_position: u8,
    },
    ErrorCode {
        code: ErrorCode,
        /// For `Unknown Mandatory Attribute`: the types not understood,
        /// one per octet as `type << 1`. Empty otherwise.
        details: Vec<u8>,
    },
    ErrorInfo(String),
    ParticipantProvidedInfo(String),
    StatusInfo(String),
    /// Attribute types, one per octet, each stored as `type << 1`.
    SupportedAttributes(Vec<u8>),
    /// Primitive values, one per octet.
    SupportedPrimitives(Vec<u8>),
    UserDisplayName(String),
    UserUri(String),
    BeneficiaryInformation {
        beneficiary_id: u16,
        attributes: Vec<Attribute>,
    },
    FloorRequestInformation {
        request_id: u16,
        attributes: Vec<Attribute>,
    },
    RequestedByInformation {
        user_id: u16,
        attributes: Vec<Attribute>,
    },
    FloorRequestStatus {
        floor_id: u16,
        attributes: Vec<Attribute>,
    },
    OverallRequestStatus {
        request_id: u16,
        attributes: Vec<Attribute>,
    },
    /// An extension attribute without the mandatory bit: kept, ignored.
    Unknown {
        attr_type: u8,
        value: Vec<u8>,
    },
}

impl Attribute {
    /// The 7-bit type on the wire.
    pub fn attr_type(&self) -> u8 {
        match self {
            Attribute::BeneficiaryId(_) => attr_type::BENEFICIARY_ID,
            Attribute::FloorId(_) => attr_type::FLOOR_ID,
            Attribute::FloorRequestId(_) => attr_type::FLOOR_REQUEST_ID,
            Attribute::Priority(_) => attr_type::PRIORITY,
            Attribute::RequestStatus { .. } => attr_type::REQUEST_STATUS,
            Attribute::ErrorCode { .. } => attr_type::ERROR_CODE,
            Attribute::ErrorInfo(_) => attr_type::ERROR_INFO,
            Attribute::ParticipantProvidedInfo(_) => attr_type::PARTICIPANT_PROVIDED_INFO,
            Attribute::StatusInfo(_) => attr_type::STATUS_INFO,
            Attribute::SupportedAttributes(_) => attr_type::SUPPORTED_ATTRIBUTES,
            Attribute::SupportedPrimitives(_) => attr_type::SUPPORTED_PRIMITIVES,
            Attribute::UserDisplayName(_) => attr_type::USER_DISPLAY_NAME,
            Attribute::UserUri(_) => attr_type::USER_URI,
            Attribute::BeneficiaryInformation { .. } => attr_type::BENEFICIARY_INFORMATION,
            Attribute::FloorRequestInformation { .. } => attr_type::FLOOR_REQUEST_INFORMATION,
            Attribute::RequestedByInformation { .. } => attr_type::REQUESTED_BY_INFORMATION,
            Attribute::FloorRequestStatus { .. } => attr_type::FLOOR_REQUEST_STATUS,
            Attribute::OverallRequestStatus { .. } => attr_type::OVERALL_REQUEST_STATUS,
            Attribute::Unknown { attr_type, .. } => *attr_type,
        }
    }

    /// The attributes inside a grouped attribute; none for the others.
    pub fn children(&self) -> &[Attribute] {
        match self {
            Attribute::BeneficiaryInformation { attributes, .. }
            | Attribute::FloorRequestInformation { attributes, .. }
            | Attribute::RequestedByInformation { attributes, .. }
            | Attribute::FloorRequestStatus { attributes, .. }
            | Attribute::OverallRequestStatus { attributes, .. } => attributes,
            _ => &[],
        }
    }

    /// The `SUPPORTED-ATTRIBUTES` this crate answers a `Hello` with.
    pub fn supported_attributes() -> Attribute {
        Attribute::SupportedAttributes(attr_type::ALL.iter().map(|t| t << 1).collect())
    }

    /// The `SUPPORTED-PRIMITIVES` this crate answers a `Hello` with.
    pub fn supported_primitives() -> Attribute {
        Attribute::SupportedPrimitives(Primitive::ALL.iter().map(|p| p.as_u8()).collect())
    }

    fn write(&self, out: &mut Vec<u8>) {
        let start = out.len();
        // Type, M and Length; the length is patched once known. Every
        // attribute this crate defines is one the RFC says a receiver
        // must understand, so the mandatory bit is set (it is only
        // significant for extensions).
        let mandatory = !matches!(self, Attribute::Unknown { .. });
        out.push((self.attr_type() << 1) | u8::from(mandatory));
        out.push(0);
        match self {
            Attribute::BeneficiaryId(v) | Attribute::FloorId(v) | Attribute::FloorRequestId(v) => {
                out.extend_from_slice(&v.to_be_bytes());
            }
            Attribute::Priority(p) => {
                out.push((*p as u8) << 5);
                out.push(0);
            }
            Attribute::RequestStatus {
                status,
                queue_position,
            } => {
                out.push(*status as u8);
                out.push(*queue_position);
            }
            Attribute::ErrorCode { code, details } => {
                out.push(code.as_u8());
                out.extend_from_slice(details);
            }
            Attribute::ErrorInfo(s)
            | Attribute::ParticipantProvidedInfo(s)
            | Attribute::StatusInfo(s)
            | Attribute::UserDisplayName(s)
            | Attribute::UserUri(s) => out.extend_from_slice(s.as_bytes()),
            Attribute::SupportedAttributes(v)
            | Attribute::SupportedPrimitives(v)
            | Attribute::Unknown { value: v, .. } => out.extend_from_slice(v),
            Attribute::BeneficiaryInformation {
                beneficiary_id: id,
                attributes,
            }
            | Attribute::FloorRequestInformation {
                request_id: id,
                attributes,
            }
            | Attribute::RequestedByInformation {
                user_id: id,
                attributes,
            }
            | Attribute::FloorRequestStatus {
                floor_id: id,
                attributes,
            }
            | Attribute::OverallRequestStatus {
                request_id: id,
                attributes,
            } => {
                out.extend_from_slice(&id.to_be_bytes());
                for a in attributes {
                    a.write(out);
                }
            }
        }
        // The length excludes padding; a grouped attribute's covers its
        // children with theirs. Padding to four octets follows.
        out[start + 1] = self.unpadded_len().min(u8::MAX as usize) as u8;
        while (out.len() - start) % 4 != 0 {
            out.push(0);
        }
    }

    /// The length the attribute's header states: the value and the two
    /// header octets, a grouped attribute's children counted with their
    /// padding.
    fn unpadded_len(&self) -> usize {
        2 + match self {
            Attribute::BeneficiaryId(_)
            | Attribute::FloorId(_)
            | Attribute::FloorRequestId(_)
            | Attribute::Priority(_)
            | Attribute::RequestStatus { .. } => 2,
            Attribute::ErrorCode { details, .. } => 1 + details.len(),
            Attribute::ErrorInfo(s)
            | Attribute::ParticipantProvidedInfo(s)
            | Attribute::StatusInfo(s)
            | Attribute::UserDisplayName(s)
            | Attribute::UserUri(s) => s.len(),
            Attribute::SupportedAttributes(v)
            | Attribute::SupportedPrimitives(v)
            | Attribute::Unknown { value: v, .. } => v.len(),
            grouped => {
                2 + grouped
                    .children()
                    .iter()
                    .map(Attribute::padded_len)
                    .sum::<usize>()
            }
        }
    }

    /// The length on the wire, padding included.
    fn padded_len(&self) -> usize {
        (self.unpadded_len() + 3) & !3
    }

    /// Whether the attribute's length fits its 8-bit field, children
    /// included.
    fn fits(&self) -> bool {
        self.unpadded_len() <= u8::MAX as usize && self.children().iter().all(Attribute::fits)
    }
}

/// The fields of a header a reply copies (RFC 8855 §8.2): what an
/// `Error` needs even when the rest of the message could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Echo {
    pub version: u8,
    pub conference_id: u32,
    pub transaction_id: u16,
    pub user_id: u16,
}

/// The common header (RFC 8855 §5.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// 1 over a reliable transport, 2 over an unreliable one.
    pub version: u8,
    /// The R flag: a response, over an unreliable transport.
    pub responder: bool,
    /// The F flag's fields: `(offset, length)` in 4-octet units.
    pub fragment: Option<(u16, u16)>,
    pub primitive: Primitive,
    pub conference_id: u32,
    pub transaction_id: u16,
    pub user_id: u16,
    /// The payload's length in octets, as the header says.
    pub payload_len: usize,
}

impl Header {
    /// Read a header from the front of `data`, without the payload. The
    /// primitive is checked; an unknown one is reported with what a
    /// reply needs.
    pub fn parse(data: &[u8]) -> Result<Header, ParseError> {
        if data.len() < COMMON_HEADER_LEN {
            return Err(ParseError::new(ParseErrorKind::Truncated, None));
        }
        let version = data[0] >> 5;
        let responder = data[0] & 0x10 != 0;
        let fragmented = data[0] & 0x08 != 0;
        let conference_id = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        let transaction_id = u16::from_be_bytes([data[8], data[9]]);
        let user_id = u16::from_be_bytes([data[10], data[11]]);
        let echo = Echo {
            version,
            conference_id,
            transaction_id,
            user_id,
        };
        let primitive = Primitive::from_u8(data[1]).ok_or_else(|| {
            ParseError::new(ParseErrorKind::UnknownPrimitive(data[1]), Some(echo))
        })?;
        let payload_len = u16::from_be_bytes([data[2], data[3]]) as usize * 4;
        let fragment = if fragmented {
            if data.len() < COMMON_HEADER_LEN + FRAGMENT_LEN {
                return Err(ParseError::new(ParseErrorKind::Truncated, Some(echo)));
            }
            Some((
                u16::from_be_bytes([data[12], data[13]]),
                u16::from_be_bytes([data[14], data[15]]),
            ))
        } else {
            None
        };
        Ok(Header {
            version,
            responder,
            fragment,
            primitive,
            conference_id,
            transaction_id,
            user_id,
            payload_len,
        })
    }

    /// The header's size on the wire: 12 octets, 16 for a fragment.
    pub fn header_len(&self) -> usize {
        COMMON_HEADER_LEN
            + if self.fragment.is_some() {
                FRAGMENT_LEN
            } else {
                0
            }
    }

    /// The whole message's size on the wire, header included.
    pub fn message_len(&self) -> usize {
        self.header_len() + self.payload_len
    }

    /// What a reply copies.
    pub fn echo(&self) -> Echo {
        Echo {
            version: self.version,
            conference_id: self.conference_id,
            transaction_id: self.transaction_id,
            user_id: self.user_id,
        }
    }
}

/// What kept a message from being read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseErrorKind {
    /// Fewer bytes than the header, or than the header says.
    Truncated,
    /// More bytes than the header says.
    TrailingBytes(usize),
    /// A primitive value the RFC does not define.
    UnknownPrimitive(u8),
    /// The types of the mandatory attributes not understood.
    UnknownMandatoryAttribute(Vec<u8>),
    /// An attribute's length runs past its message, or under its header.
    BadAttributeLength(u8),
    /// An attribute's value is not what its type allows.
    BadAttributeValue(u8),
    /// A fragment of a larger message, which this crate does not
    /// reassemble.
    Fragmented,
}

/// A message that could not be read, with what an `Error` reply needs
/// when the header could be.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{kind:?}")]
pub struct ParseError {
    pub kind: ParseErrorKind,
    pub echo: Option<Echo>,
}

impl ParseError {
    fn new(kind: ParseErrorKind, echo: Option<Echo>) -> ParseError {
        ParseError { kind, echo }
    }

    /// The error code a floor server answers this with (RFC 8855 §13).
    pub fn error_code(&self) -> ErrorCode {
        match &self.kind {
            ParseErrorKind::UnknownPrimitive(_) => ErrorCode::UnknownPrimitive,
            ParseErrorKind::UnknownMandatoryAttribute(_) => ErrorCode::UnknownMandatoryAttribute,
            ParseErrorKind::Truncated | ParseErrorKind::TrailingBytes(_) => {
                ErrorCode::IncorrectMessageLength
            }
            ParseErrorKind::BadAttributeLength(_)
            | ParseErrorKind::BadAttributeValue(_)
            | ParseErrorKind::Fragmented => ErrorCode::UnableToParseMessage,
        }
    }
}

/// A message could not be written.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EncodeError {
    #[error("an attribute of type {0} is longer than its length field can say")]
    AttributeTooLong(u8),
    #[error("the message's payload is longer than its length field can say")]
    PayloadTooLong,
}

/// One BFCP message: a header and its attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub version: u8,
    /// The R flag.
    pub responder: bool,
    pub primitive: Primitive,
    pub conference_id: u32,
    pub transaction_id: u16,
    pub user_id: u16,
    pub attributes: Vec<Attribute>,
}

impl Message {
    /// A message with no attributes yet.
    pub fn new(
        version: u8,
        primitive: Primitive,
        conference_id: u32,
        transaction_id: u16,
        user_id: u16,
    ) -> Message {
        Message {
            version,
            responder: false,
            primitive,
            conference_id,
            transaction_id,
            user_id,
            attributes: Vec::new(),
        }
    }

    /// A response in the transaction `echo` names (RFC 8855 §8.2): the
    /// same conference, transaction and user, the R flag over an
    /// unreliable transport.
    pub fn response(echo: Echo, primitive: Primitive) -> Message {
        Message {
            version: echo.version,
            responder: echo.version == VERSION_UNRELIABLE,
            primitive,
            conference_id: echo.conference_id,
            transaction_id: echo.transaction_id,
            user_id: echo.user_id,
            attributes: Vec::new(),
        }
    }

    /// An `Error` response (§13.8).
    pub fn error(echo: Echo, code: ErrorCode, info: Option<&str>) -> Message {
        let mut m = Message::response(echo, Primitive::Error);
        m.attributes.push(Attribute::ErrorCode {
            code,
            details: Vec::new(),
        });
        if let Some(info) = info {
            m.attributes.push(Attribute::ErrorInfo(info.to_string()));
        }
        m
    }

    pub fn with(mut self, attribute: Attribute) -> Message {
        self.attributes.push(attribute);
        self
    }

    /// What a reply to this message copies.
    pub fn echo(&self) -> Echo {
        Echo {
            version: self.version,
            conference_id: self.conference_id,
            transaction_id: self.transaction_id,
            user_id: self.user_id,
        }
    }

    /// Read one message that fills `data` exactly (a datagram, or a
    /// frame the [`TcpFramer`](crate::transport::TcpFramer) cut).
    pub fn parse(data: &[u8]) -> Result<Message, ParseError> {
        let (message, used) = Message::parse_prefix(data)?;
        if used != data.len() {
            return Err(ParseError::new(
                ParseErrorKind::TrailingBytes(data.len() - used),
                Some(message.echo()),
            ));
        }
        Ok(message)
    }

    /// Read the message at the front of `data`, returning it and how many
    /// bytes it took.
    pub fn parse_prefix(data: &[u8]) -> Result<(Message, usize), ParseError> {
        let header = Header::parse(data)?;
        let echo = header.echo();
        if header.fragment.is_some() {
            return Err(ParseError::new(ParseErrorKind::Fragmented, Some(echo)));
        }
        let start = header.header_len();
        let end = start + header.payload_len;
        if data.len() < end {
            return Err(ParseError::new(ParseErrorKind::Truncated, Some(echo)));
        }
        let mut unknown_mandatory = Vec::new();
        let attributes = parse_attributes(&data[start..end], &mut unknown_mandatory, echo, 0)?;
        if !unknown_mandatory.is_empty() {
            return Err(ParseError::new(
                ParseErrorKind::UnknownMandatoryAttribute(unknown_mandatory),
                Some(echo),
            ));
        }
        Ok((
            Message {
                version: header.version,
                responder: header.responder,
                primitive: header.primitive,
                conference_id: header.conference_id,
                transaction_id: header.transaction_id,
                user_id: header.user_id,
                attributes,
            },
            end,
        ))
    }

    /// The message on the wire.
    pub fn to_bytes(&self) -> Result<Vec<u8>, EncodeError> {
        let mut body = Vec::new();
        for a in &self.attributes {
            if !a.fits() {
                return Err(EncodeError::AttributeTooLong(a.attr_type()));
            }
            a.write(&mut body);
        }
        if body.len() > MAX_PAYLOAD_LEN {
            return Err(EncodeError::PayloadTooLong);
        }
        let mut out = Vec::with_capacity(COMMON_HEADER_LEN + body.len());
        out.push((self.version & 0x07) << 5 | if self.responder { 0x10 } else { 0 });
        out.push(self.primitive.as_u8());
        out.extend_from_slice(&((body.len() / 4) as u16).to_be_bytes());
        out.extend_from_slice(&self.conference_id.to_be_bytes());
        out.extend_from_slice(&self.transaction_id.to_be_bytes());
        out.extend_from_slice(&self.user_id.to_be_bytes());
        out.extend_from_slice(&body);
        Ok(out)
    }

    // ---- the attributes a floor server reads --------------------------

    /// Every `FLOOR-ID` at the top level.
    pub fn floor_ids(&self) -> Vec<u16> {
        self.attributes
            .iter()
            .filter_map(|a| match a {
                Attribute::FloorId(id) => Some(*id),
                _ => None,
            })
            .collect()
    }

    /// The `FLOOR-REQUEST-ID`, if any.
    pub fn floor_request_id(&self) -> Option<u16> {
        self.attributes.iter().find_map(|a| match a {
            Attribute::FloorRequestId(id) => Some(*id),
            _ => None,
        })
    }

    /// The `BENEFICIARY-ID`, if any.
    pub fn beneficiary_id(&self) -> Option<u16> {
        self.attributes.iter().find_map(|a| match a {
            Attribute::BeneficiaryId(id) => Some(*id),
            _ => None,
        })
    }

    /// The `PRIORITY`, `Normal` when absent.
    pub fn priority(&self) -> Priority {
        self.attributes
            .iter()
            .find_map(|a| match a {
                Attribute::Priority(p) => Some(*p),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// The `PARTICIPANT-PROVIDED-INFO`, if any.
    pub fn participant_provided_info(&self) -> Option<&str> {
        self.attributes.iter().find_map(|a| match a {
            Attribute::ParticipantProvidedInfo(s) => Some(s.as_str()),
            _ => None,
        })
    }

    /// The `ERROR-CODE` of an `Error`, if any.
    pub fn error_code(&self) -> Option<ErrorCode> {
        self.attributes.iter().find_map(|a| match a {
            Attribute::ErrorCode { code, .. } => Some(*code),
            _ => None,
        })
    }

    /// The `FLOOR-REQUEST-INFORMATION` groups, with their request ids.
    pub fn floor_request_information(&self) -> Vec<(u16, &[Attribute])> {
        self.attributes
            .iter()
            .filter_map(|a| match a {
                Attribute::FloorRequestInformation {
                    request_id,
                    attributes,
                } => Some((*request_id, attributes.as_slice())),
                _ => None,
            })
            .collect()
    }
}

/// The overall status inside a `FLOOR-REQUEST-INFORMATION`, if any.
pub fn overall_status(info: &[Attribute]) -> Option<RequestStatus> {
    info.iter().find_map(|a| match a {
        Attribute::OverallRequestStatus { attributes, .. } => {
            attributes.iter().find_map(|b| match b {
                Attribute::RequestStatus { status, .. } => Some(*status),
                _ => None,
            })
        }
        _ => None,
    })
}

/// The longest value an attribute's 8-bit length field allows.
const MAX_VALUE_LEN: usize = u8::MAX as usize - 2;

/// A text attribute's value. The RFC says UTF-8; what is not is decoded
/// with replacement characters, and since those are wider than the bytes
/// they replace, the result is cut to what the attribute's length field
/// could carry again, so a message read can always be written.
fn text_of(v: &[u8]) -> String {
    let mut s = String::from_utf8_lossy(v).into_owned();
    if s.len() > MAX_VALUE_LEN {
        let mut cut = MAX_VALUE_LEN;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    s
}

/// How deep grouped attributes may nest: the RFC nests two levels; a
/// hostile message does not get to nest forever.
const MAX_DEPTH: usize = 4;

fn parse_attributes(
    mut data: &[u8],
    unknown_mandatory: &mut Vec<u8>,
    echo: Echo,
    depth: usize,
) -> Result<Vec<Attribute>, ParseError> {
    let mut out = Vec::new();
    while !data.is_empty() {
        if data.len() < 2 {
            return Err(ParseError::new(ParseErrorKind::Truncated, Some(echo)));
        }
        let attr_type = data[0] >> 1;
        let mandatory = data[0] & 1 != 0;
        let len = data[1] as usize;
        if len < 2 || len > data.len() {
            return Err(ParseError::new(
                ParseErrorKind::BadAttributeLength(attr_type),
                Some(echo),
            ));
        }
        let value = &data[2..len];
        let bad = || ParseError::new(ParseErrorKind::BadAttributeValue(attr_type), Some(echo));
        let u16_of = |v: &[u8]| -> Result<u16, ParseError> {
            if v.len() < 2 {
                Err(bad())
            } else {
                Ok(u16::from_be_bytes([v[0], v[1]]))
            }
        };
        let text = text_of;
        let grouped = |v: &[u8],
                       unknown_mandatory: &mut Vec<u8>|
         -> Result<(u16, Vec<Attribute>), ParseError> {
            if depth >= MAX_DEPTH {
                return Err(bad());
            }
            let id = u16_of(v)?;
            let inner = parse_attributes(&v[2..], unknown_mandatory, echo, depth + 1)?;
            Ok((id, inner))
        };
        let attribute = match attr_type {
            attr_type::BENEFICIARY_ID => Attribute::BeneficiaryId(u16_of(value)?),
            attr_type::FLOOR_ID => Attribute::FloorId(u16_of(value)?),
            attr_type::FLOOR_REQUEST_ID => Attribute::FloorRequestId(u16_of(value)?),
            attr_type::PRIORITY => {
                if value.len() < 2 {
                    return Err(bad());
                }
                Attribute::Priority(Priority::from_u8(value[0] >> 5))
            }
            attr_type::REQUEST_STATUS => {
                if value.len() < 2 {
                    return Err(bad());
                }
                Attribute::RequestStatus {
                    status: RequestStatus::from_u8(value[0]).ok_or_else(bad)?,
                    queue_position: value[1],
                }
            }
            attr_type::ERROR_CODE => {
                if value.is_empty() {
                    return Err(bad());
                }
                Attribute::ErrorCode {
                    code: ErrorCode::from_u8(value[0]),
                    details: value[1..].to_vec(),
                }
            }
            attr_type::ERROR_INFO => Attribute::ErrorInfo(text(value)),
            attr_type::PARTICIPANT_PROVIDED_INFO => Attribute::ParticipantProvidedInfo(text(value)),
            attr_type::STATUS_INFO => Attribute::StatusInfo(text(value)),
            attr_type::SUPPORTED_ATTRIBUTES => Attribute::SupportedAttributes(value.to_vec()),
            attr_type::SUPPORTED_PRIMITIVES => Attribute::SupportedPrimitives(value.to_vec()),
            attr_type::USER_DISPLAY_NAME => Attribute::UserDisplayName(text(value)),
            attr_type::USER_URI => Attribute::UserUri(text(value)),
            attr_type::BENEFICIARY_INFORMATION => {
                let (beneficiary_id, attributes) = grouped(value, unknown_mandatory)?;
                Attribute::BeneficiaryInformation {
                    beneficiary_id,
                    attributes,
                }
            }
            attr_type::FLOOR_REQUEST_INFORMATION => {
                let (request_id, attributes) = grouped(value, unknown_mandatory)?;
                Attribute::FloorRequestInformation {
                    request_id,
                    attributes,
                }
            }
            attr_type::REQUESTED_BY_INFORMATION => {
                let (user_id, attributes) = grouped(value, unknown_mandatory)?;
                Attribute::RequestedByInformation {
                    user_id,
                    attributes,
                }
            }
            attr_type::FLOOR_REQUEST_STATUS => {
                let (floor_id, attributes) = grouped(value, unknown_mandatory)?;
                Attribute::FloorRequestStatus {
                    floor_id,
                    attributes,
                }
            }
            attr_type::OVERALL_REQUEST_STATUS => {
                let (request_id, attributes) = grouped(value, unknown_mandatory)?;
                Attribute::OverallRequestStatus {
                    request_id,
                    attributes,
                }
            }
            other => {
                if mandatory {
                    unknown_mandatory.push(other);
                }
                Attribute::Unknown {
                    attr_type: other,
                    value: value.to_vec(),
                }
            }
        };
        out.push(attribute);
        // On to the next four-octet boundary; the last attribute's
        // padding may be cut short by the message's end.
        let padded = (len + 3) & !3;
        data = &data[padded.min(data.len())..];
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_floor_request_round_trips_and_matches_the_rfc_layout() {
        let m = Message::new(VERSION_UNRELIABLE, Primitive::FloorRequest, 4321, 7, 1234)
            .with(Attribute::FloorId(1))
            .with(Attribute::Priority(Priority::High))
            .with(Attribute::ParticipantProvidedInfo("slides".into()));
        let bytes = m.to_bytes().unwrap();
        // Ver 2, R 0, F 0; FloorRequest; 4 words; conference; txn; user.
        assert_eq!(
            &bytes[..12],
            &[0x40, 1, 0, 4, 0, 0, 0x10, 0xE1, 0, 7, 0x04, 0xD2]
        );
        // FLOOR-ID: type 2 with M, length 4, value 1.
        assert_eq!(&bytes[12..16], &[0x05, 4, 0, 1]);
        // PRIORITY: type 4 with M, length 4, High in the top three bits.
        assert_eq!(&bytes[16..20], &[0x09, 4, 0x60, 0]);
        // PARTICIPANT-PROVIDED-INFO: type 8 with M, length 2 + 6, padded to 8.
        assert_eq!(
            &bytes[20..28],
            &[0x11, 8, b's', b'l', b'i', b'd', b'e', b's']
        );
        assert_eq!(bytes.len(), 28);
        assert_eq!(Message::parse(&bytes).unwrap(), m);
    }

    #[test]
    fn grouped_attributes_nest_and_pad() {
        let m = Message::new(VERSION_RELIABLE, Primitive::FloorRequestStatus, 1, 0, 9).with(
            Attribute::FloorRequestInformation {
                request_id: 3,
                attributes: vec![
                    Attribute::OverallRequestStatus {
                        request_id: 3,
                        attributes: vec![
                            Attribute::RequestStatus {
                                status: RequestStatus::Granted,
                                queue_position: 0,
                            },
                            Attribute::StatusInfo("ok".into()),
                        ],
                    },
                    Attribute::FloorRequestStatus {
                        floor_id: 1,
                        attributes: vec![Attribute::RequestStatus {
                            status: RequestStatus::Granted,
                            queue_position: 0,
                        }],
                    },
                    Attribute::BeneficiaryInformation {
                        beneficiary_id: 9,
                        attributes: vec![Attribute::UserDisplayName("Ann".into())],
                    },
                ],
            },
        );
        let bytes = m.to_bytes().unwrap();
        assert_eq!(bytes.len() % 4, 0);
        let back = Message::parse(&bytes).unwrap();
        assert_eq!(back, m);
        assert_eq!(
            overall_status(back.floor_request_information()[0].1),
            Some(RequestStatus::Granted)
        );
        // A grouped length covers its children with their padding: the
        // OVERALL-REQUEST-STATUS holds 4 + 4 + (2 + 2 padded to 4) = 12.
        assert_eq!(bytes[16], (attr_type::OVERALL_REQUEST_STATUS << 1) | 1);
        assert_eq!(bytes[17], 12);
    }

    #[test]
    fn hostile_bytes_are_refused_not_panicked_on() {
        assert_eq!(
            Message::parse(&[0x40, 1, 0]).unwrap_err().kind,
            ParseErrorKind::Truncated
        );
        // Payload longer than the datagram.
        let mut short = Message::new(2, Primitive::Hello, 1, 1, 1)
            .to_bytes()
            .unwrap();
        short[3] = 5;
        let err = Message::parse(&short).unwrap_err();
        assert_eq!(err.kind, ParseErrorKind::Truncated);
        assert_eq!(err.echo.unwrap().transaction_id, 1);
        assert_eq!(err.error_code(), ErrorCode::IncorrectMessageLength);
        // An unknown primitive still yields what an Error needs.
        let mut unknown = Message::new(2, Primitive::Hello, 1, 2, 3)
            .to_bytes()
            .unwrap();
        unknown[1] = 200;
        let err = Message::parse(&unknown).unwrap_err();
        assert_eq!(err.kind, ParseErrorKind::UnknownPrimitive(200));
        assert_eq!(err.error_code(), ErrorCode::UnknownPrimitive);
        assert_eq!(err.echo.unwrap().user_id, 3);
        // An attribute whose length runs off the end.
        let mut bad = Message::new(2, Primitive::FloorRequest, 1, 1, 1)
            .with(Attribute::FloorId(1))
            .to_bytes()
            .unwrap();
        bad[13] = 200;
        assert_eq!(
            Message::parse(&bad).unwrap_err().kind,
            ParseErrorKind::BadAttributeLength(attr_type::FLOOR_ID)
        );
        bad[13] = 1;
        assert_eq!(
            Message::parse(&bad).unwrap_err().kind,
            ParseErrorKind::BadAttributeLength(attr_type::FLOOR_ID)
        );
        // A fragment is not reassembled.
        let mut frag = Message::new(2, Primitive::Hello, 1, 1, 1)
            .to_bytes()
            .unwrap();
        frag[0] |= 0x08;
        frag.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(
            Message::parse(&frag).unwrap_err().kind,
            ParseErrorKind::Fragmented
        );
        // Trailing bytes are not silently ignored.
        let mut long = Message::new(2, Primitive::Hello, 1, 1, 1)
            .to_bytes()
            .unwrap();
        long.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(
            Message::parse(&long).unwrap_err().kind,
            ParseErrorKind::TrailingBytes(4)
        );
    }

    #[test]
    fn unknown_attributes_are_kept_unless_mandatory() {
        let m = Message::new(2, Primitive::Hello, 1, 1, 1).with(Attribute::Unknown {
            attr_type: 100,
            value: vec![1, 2, 3],
        });
        let bytes = m.to_bytes().unwrap();
        assert_eq!(Message::parse(&bytes).unwrap(), m);
        let mut mandatory = bytes.clone();
        mandatory[12] |= 1;
        let err = Message::parse(&mandatory).unwrap_err();
        assert_eq!(
            err.kind,
            ParseErrorKind::UnknownMandatoryAttribute(vec![100])
        );
        assert_eq!(err.error_code(), ErrorCode::UnknownMandatoryAttribute);
    }

    #[test]
    fn responses_copy_the_transaction_and_set_r_over_udp() {
        let req = Message::new(VERSION_UNRELIABLE, Primitive::Hello, 5, 77, 8);
        let ack = Message::response(req.echo(), Primitive::HelloAck);
        assert!(ack.responder);
        assert_eq!(
            (ack.conference_id, ack.transaction_id, ack.user_id),
            (5, 77, 8)
        );
        let bytes = ack.to_bytes().unwrap();
        assert_eq!(bytes[0], 0x50);
        let tcp = Message::response(
            Message::new(VERSION_RELIABLE, Primitive::Hello, 5, 77, 8).echo(),
            Primitive::HelloAck,
        );
        assert!(!tcp.responder);
        assert_eq!(tcp.to_bytes().unwrap()[0], 0x20);
        let err = Message::error(req.echo(), ErrorCode::UnknownPrimitive, Some("no"));
        assert_eq!(err.error_code(), Some(ErrorCode::UnknownPrimitive));
        let back = Message::parse(&err.to_bytes().unwrap()).unwrap();
        assert!(matches!(&back.attributes[1], Attribute::ErrorInfo(s) if s == "no"));
    }

    #[test]
    fn an_attribute_too_long_for_its_length_field_is_refused() {
        let m =
            Message::new(2, Primitive::Hello, 1, 1, 1).with(Attribute::StatusInfo("x".repeat(300)));
        assert_eq!(
            m.to_bytes().unwrap_err(),
            EncodeError::AttributeTooLong(attr_type::STATUS_INFO)
        );
        let ok =
            Message::new(2, Primitive::Hello, 1, 1, 1).with(Attribute::StatusInfo("x".repeat(253)));
        assert!(ok.to_bytes().is_ok());
    }

    #[test]
    fn text_that_is_not_utf8_still_round_trips() {
        // 253 invalid bytes decode to 253 replacement characters, three
        // bytes each: far more than the length field can say. What is
        // read is cut to fit, and reads back the same.
        let mut bytes = Message::new(2, Primitive::Error, 1, 1, 1)
            .with(Attribute::ErrorInfo("x".repeat(253)))
            .to_bytes()
            .unwrap();
        for b in &mut bytes[14..14 + 253] {
            *b = 0xFF;
        }
        let m = Message::parse(&bytes).unwrap();
        let Attribute::ErrorInfo(s) = &m.attributes[0] else {
            unreachable!()
        };
        assert!(s.len() <= 253);
        assert!(s.starts_with('\u{FFFD}'));
        let again = m.to_bytes().unwrap();
        assert_eq!(Message::parse(&again).unwrap(), m);
    }

    #[test]
    fn hello_ack_lists_everything_this_crate_speaks() {
        let attrs = Attribute::supported_attributes();
        let prims = Attribute::supported_primitives();
        let Attribute::SupportedAttributes(a) = &attrs else {
            unreachable!()
        };
        assert_eq!(a[0], attr_type::BENEFICIARY_ID << 1);
        assert_eq!(a.len(), 18);
        let Attribute::SupportedPrimitives(p) = &prims else {
            unreachable!()
        };
        assert_eq!(p.len(), 17);
        assert_eq!(Primitive::from_u8(p[16]), Some(Primitive::GoodbyeAck));
        assert!(Primitive::FloorStatusAck.is_ack());
        assert!(!Primitive::FloorStatus.is_ack());
    }
}
