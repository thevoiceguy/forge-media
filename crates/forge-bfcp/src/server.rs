//! A floor server for one floor (RFC 8855 §13), with the decisions left
//! to whoever owns the floor.
//!
//! The [`FloorServer`] takes each client message as bytes, answers what
//! the RFC says to answer, and reports what it cannot decide as an
//! [`Event`]: a `FloorRequest` becomes a pending request and an
//! [`Event::Requested`], and the owner — a conference's content floor —
//! calls [`grant`](FloorServer::grant), [`deny`](FloorServer::deny) or,
//! later, [`revoke`](FloorServer::revoke). The server then tells the
//! requester with a `FloorRequestStatus` and everyone who asked with
//! `FloorQuery` with a `FloorStatus`. Those server-initiated messages
//! come back as [`Notification`]s; over UDP they need acknowledging, which
//! is [`UdpPeer`](crate::transport::UdpPeer)'s business.
//!
//! One floor, one ongoing request per user, no chairs: the conference's
//! host is the chair through its own API, and a `ChairAction` draws
//! `Unauthorized Operation`.

use std::collections::BTreeMap;

use crate::message::{
    overall_status, Attribute, Echo, ErrorCode, Message, ParseError, Primitive, Priority,
    RequestStatus, VERSION_RELIABLE, VERSION_UNRELIABLE,
};
use tracing::debug;

/// What the messages ride on: it decides the version they carry and
/// whether server-initiated ones need a transaction id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// TCP: version 1, notifications with transaction id 0.
    Reliable,
    /// UDP: version 2, notifications with a fresh transaction id that
    /// the client acknowledges.
    Unreliable,
}

impl Transport {
    pub fn version(self) -> u8 {
        match self {
            Transport::Reliable => VERSION_RELIABLE,
            Transport::Unreliable => VERSION_UNRELIABLE,
        }
    }
}

/// A user the server knows: the conference assigns the id (it goes in
/// the SDP `a=userid`) and may name them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct User {
    display_name: Option<String>,
    uri: Option<String>,
    /// Asked for `FloorStatus` on the floor with a `FloorQuery`.
    subscribed: bool,
}

/// A floor request the server holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub id: u16,
    pub user_id: u16,
    pub status: RequestStatus,
    pub priority: Priority,
    /// The requester's `PARTICIPANT-PROVIDED-INFO`.
    pub info: Option<String>,
}

/// What the owner needs to know after a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A client announced itself.
    Hello { user_id: u16 },
    /// A client asked for the floor: the request is pending until the
    /// owner grants or denies it.
    Requested {
        user_id: u16,
        request_id: u16,
        priority: Priority,
        info: Option<String>,
    },
    /// A client gave the floor back (`was_granted`) or withdrew a
    /// pending request.
    Released {
        user_id: u16,
        request_id: u16,
        was_granted: bool,
    },
    /// A client acknowledged a server-initiated transaction (UDP).
    Acked { user_id: u16, transaction_id: u16 },
    /// A client said goodbye; its requests are gone (`released` names
    /// the granted one, if any).
    Goodbye { user_id: u16, released: Option<u16> },
}

/// A server-initiated message for one user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub user_id: u16,
    pub message: Message,
}

/// What handling a message produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Handled {
    /// The response, to the sender, in the sender's transaction.
    pub reply: Option<Message>,
    /// Server-initiated messages that follow from it.
    pub notifications: Vec<Notification>,
    pub events: Vec<Event>,
}

/// The floor server: one conference, one floor.
#[derive(Debug)]
pub struct FloorServer {
    conference_id: u32,
    floor_id: u16,
    transport: Transport,
    users: BTreeMap<u16, User>,
    requests: BTreeMap<u16, Request>,
    next_request_id: u16,
    next_transaction_id: u16,
}

impl FloorServer {
    pub fn new(conference_id: u32, floor_id: u16, transport: Transport) -> FloorServer {
        FloorServer {
            conference_id,
            floor_id,
            transport,
            users: BTreeMap::new(),
            requests: BTreeMap::new(),
            next_request_id: 1,
            next_transaction_id: 1,
        }
    }

    pub fn conference_id(&self) -> u32 {
        self.conference_id
    }

    pub fn floor_id(&self) -> u16 {
        self.floor_id
    }

    pub fn transport(&self) -> Transport {
        self.transport
    }

    /// A user the conference admitted, under the id its SDP told them.
    pub fn add_user(&mut self, user_id: u16, display_name: Option<&str>, uri: Option<&str>) {
        self.users.insert(
            user_id,
            User {
                display_name: display_name.map(str::to_string),
                uri: uri.map(str::to_string),
                subscribed: false,
            },
        );
    }

    /// The user left the conference: their requests go, the floor they
    /// held is free, and the others hear it. Returns the notifications
    /// and the released request's id, if they held the floor.
    pub fn remove_user(&mut self, user_id: u16) -> (Vec<Notification>, Option<u16>) {
        self.users.remove(&user_id);
        let theirs: Vec<u16> = self
            .requests
            .values()
            .filter(|r| r.user_id == user_id)
            .map(|r| r.id)
            .collect();
        let mut released = None;
        for id in theirs {
            if let Some(r) = self.requests.remove(&id) {
                if r.status == RequestStatus::Granted {
                    released = Some(id);
                }
            }
        }
        let notifications = if released.is_some() {
            self.floor_status_to_subscribers()
        } else {
            Vec::new()
        };
        (notifications, released)
    }

    /// Who holds the floor: `(user, request)`.
    pub fn holder(&self) -> Option<(u16, u16)> {
        self.requests
            .values()
            .find(|r| r.status == RequestStatus::Granted)
            .map(|r| (r.user_id, r.id))
    }

    /// Every ongoing request, oldest first.
    pub fn requests(&self) -> impl Iterator<Item = &Request> {
        self.requests.values()
    }

    /// The request, if the server still holds it.
    pub fn request(&self, request_id: u16) -> Option<&Request> {
        self.requests.get(&request_id)
    }

    // ---- inbound ------------------------------------------------------

    /// Handle one message from a client. A message that cannot be read
    /// is answered with the `Error` RFC 8855 §13 asks for whenever the
    /// header could be; one that cannot even yield that is dropped.
    pub fn handle(&mut self, data: &[u8]) -> Handled {
        match Message::parse(data) {
            Ok(m) => self.handle_message(m),
            Err(ParseError { kind, echo }) => {
                debug!(?kind, "BFCP message refused");
                let mut out = Handled::default();
                if let Some(echo) = echo {
                    let err = ParseError {
                        kind,
                        echo: Some(echo),
                    };
                    let details = match &err.kind {
                        crate::message::ParseErrorKind::UnknownMandatoryAttribute(types) => {
                            types.iter().map(|t| t << 1).collect()
                        }
                        _ => Vec::new(),
                    };
                    let mut reply = Message::error(echo, err.error_code(), None);
                    if let Some(Attribute::ErrorCode { details: d, .. }) =
                        reply.attributes.first_mut()
                    {
                        *d = details;
                    }
                    out.reply = Some(reply);
                }
                out
            }
        }
    }

    /// Handle a message already parsed.
    pub fn handle_message(&mut self, m: Message) -> Handled {
        let echo = m.echo();
        let mut out = Handled::default();
        let refuse = |code: ErrorCode, info: &str| Handled {
            reply: Some(Message::error(echo, code, Some(info))),
            ..Default::default()
        };
        if m.version != self.transport.version() {
            return refuse(
                ErrorCode::UnsupportedVersion,
                &format!("version {} on this transport", self.transport.version()),
            );
        }
        if m.conference_id != self.conference_id {
            return refuse(ErrorCode::ConferenceDoesNotExist, "no such conference");
        }
        // Acknowledgements and other responses carry no user check: they
        // close transactions, they do not start any.
        if m.primitive.is_ack() {
            out.events.push(Event::Acked {
                user_id: m.user_id,
                transaction_id: m.transaction_id,
            });
            return out;
        }
        if !self.users.contains_key(&m.user_id) {
            return refuse(ErrorCode::UserDoesNotExist, "no such user");
        }
        match m.primitive {
            Primitive::Hello => {
                out.reply = Some(
                    Message::response(echo, Primitive::HelloAck)
                        .with(Attribute::supported_primitives())
                        .with(Attribute::supported_attributes()),
                );
                out.events.push(Event::Hello { user_id: m.user_id });
            }
            Primitive::FloorRequest => {
                let floors = m.floor_ids();
                if floors.is_empty() || floors.iter().any(|f| *f != self.floor_id) {
                    return refuse(ErrorCode::InvalidFloorId, "this conference has one floor");
                }
                if let Some(b) = m.beneficiary_id() {
                    if b != m.user_id {
                        return refuse(
                            ErrorCode::UnauthorizedOperation,
                            "a request on another's behalf needs a chair",
                        );
                    }
                }
                if self.requests.values().any(|r| r.user_id == m.user_id) {
                    return refuse(
                        ErrorCode::MaximumFloorRequestsReached,
                        "one ongoing request per user",
                    );
                }
                let id = self.fresh_request_id();
                let request = Request {
                    id,
                    user_id: m.user_id,
                    status: RequestStatus::Pending,
                    priority: m.priority(),
                    info: m.participant_provided_info().map(str::to_string),
                };
                self.requests.insert(id, request.clone());
                out.reply = Some(self.request_status(echo, &request, None));
                out.events.push(Event::Requested {
                    user_id: m.user_id,
                    request_id: id,
                    priority: request.priority,
                    info: request.info,
                });
            }
            Primitive::FloorRelease => {
                let Some(id) = m.floor_request_id() else {
                    return refuse(ErrorCode::FloorRequestIdDoesNotExist, "no FLOOR-REQUEST-ID");
                };
                let Some(request) = self.requests.get(&id).cloned() else {
                    return refuse(ErrorCode::FloorRequestIdDoesNotExist, "no such request");
                };
                if request.user_id != m.user_id {
                    return refuse(ErrorCode::UnauthorizedOperation, "not your request");
                }
                let was_granted = request.status == RequestStatus::Granted;
                let mut done = request;
                done.status = if was_granted {
                    RequestStatus::Released
                } else {
                    RequestStatus::Cancelled
                };
                self.requests.remove(&id);
                out.reply = Some(self.request_status(echo, &done, None));
                out.events.push(Event::Released {
                    user_id: m.user_id,
                    request_id: id,
                    was_granted,
                });
                if was_granted {
                    out.notifications = self.floor_status_to_subscribers();
                }
            }
            Primitive::FloorRequestQuery => {
                let Some(request) = m.floor_request_id().and_then(|id| self.requests.get(&id))
                else {
                    return refuse(ErrorCode::FloorRequestIdDoesNotExist, "no such request");
                };
                out.reply = Some(self.request_status(echo, &request.clone(), None));
            }
            Primitive::FloorQuery => {
                let floors = m.floor_ids();
                if floors.iter().any(|f| *f != self.floor_id) {
                    return refuse(ErrorCode::InvalidFloorId, "this conference has one floor");
                }
                let subscribed = !floors.is_empty();
                if let Some(u) = self.users.get_mut(&m.user_id) {
                    u.subscribed = subscribed;
                }
                out.reply = Some(if subscribed {
                    self.floor_status(echo)
                } else {
                    Message::response(echo, Primitive::FloorStatus)
                });
            }
            Primitive::UserQuery => {
                let about = m.beneficiary_id().unwrap_or(m.user_id);
                if !self.users.contains_key(&about) {
                    return refuse(ErrorCode::UserDoesNotExist, "no such user");
                }
                let mut reply = Message::response(echo, Primitive::UserStatus)
                    .with(self.beneficiary_information(about));
                for r in self.requests.values().filter(|r| r.user_id == about) {
                    reply
                        .attributes
                        .push(self.floor_request_information(r, None));
                }
                out.reply = Some(reply);
            }
            Primitive::ChairAction => {
                return refuse(ErrorCode::UnauthorizedOperation, "the host is the chair");
            }
            Primitive::Goodbye => {
                out.reply = Some(Message::response(echo, Primitive::GoodbyeAck));
                let (notifications, released) = self.remove_user(m.user_id);
                out.notifications = notifications;
                out.events.push(Event::Goodbye {
                    user_id: m.user_id,
                    released,
                });
            }
            // A server's own primitives from a client: nothing to do.
            Primitive::FloorRequestStatus
            | Primitive::UserStatus
            | Primitive::FloorStatus
            | Primitive::ChairActionAck
            | Primitive::HelloAck
            | Primitive::Error
            | Primitive::FloorRequestStatusAck
            | Primitive::FloorStatusAck
            | Primitive::GoodbyeAck => {
                debug!(primitive = %m.primitive, user = m.user_id, "ignoring a server primitive from a client");
            }
        }
        out
    }

    // ---- the owner's decisions ------------------------------------------

    /// The floor is the request's: tell the requester and the subscribers.
    pub fn grant(&mut self, request_id: u16) -> Vec<Notification> {
        self.decide(request_id, RequestStatus::Granted, None)
    }

    /// The request is refused, with a reason for the requester.
    pub fn deny(&mut self, request_id: u16, info: Option<&str>) -> Vec<Notification> {
        self.decide(request_id, RequestStatus::Denied, info)
    }

    /// The floor is taken away from its holder — a host's stop.
    pub fn revoke(&mut self, request_id: u16, info: Option<&str>) -> Vec<Notification> {
        self.decide(request_id, RequestStatus::Revoked, info)
    }

    fn decide(
        &mut self,
        request_id: u16,
        status: RequestStatus,
        info: Option<&str>,
    ) -> Vec<Notification> {
        let Some(mut request) = self.requests.get(&request_id).cloned() else {
            return Vec::new();
        };
        request.status = status;
        if status.is_terminal() {
            self.requests.remove(&request_id);
        } else {
            self.requests.insert(request_id, request.clone());
        }
        let mut out = Vec::new();
        let echo = self.notification_echo(request.user_id);
        out.push(Notification {
            user_id: request.user_id,
            message: self.request_status(echo, &request, info),
        });
        out.extend(self.floor_status_to_subscribers());
        out
    }

    // ---- messages -------------------------------------------------------

    fn fresh_request_id(&mut self) -> u16 {
        loop {
            let id = self.next_request_id;
            self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
            if !self.requests.contains_key(&id) {
                return id;
            }
        }
    }

    /// The header of a server-initiated message to `user_id`: transaction
    /// 0 over a reliable transport, a fresh non-zero one over UDP
    /// (RFC 8855 §8.2).
    fn notification_echo(&mut self, user_id: u16) -> Echo {
        let transaction_id = match self.transport {
            Transport::Reliable => 0,
            Transport::Unreliable => {
                let id = self.next_transaction_id;
                self.next_transaction_id = self.next_transaction_id.wrapping_add(1).max(1);
                id
            }
        };
        Echo {
            version: self.transport.version(),
            conference_id: self.conference_id,
            transaction_id,
            user_id,
        }
    }

    fn beneficiary_information(&self, user_id: u16) -> Attribute {
        let mut attributes = Vec::new();
        if let Some(u) = self.users.get(&user_id) {
            if let Some(n) = &u.display_name {
                attributes.push(Attribute::UserDisplayName(n.clone()));
            }
            if let Some(uri) = &u.uri {
                attributes.push(Attribute::UserUri(uri.clone()));
            }
        }
        Attribute::BeneficiaryInformation {
            beneficiary_id: user_id,
            attributes,
        }
    }

    fn floor_request_information(&self, r: &Request, info: Option<&str>) -> Attribute {
        let mut overall = vec![Attribute::RequestStatus {
            status: r.status,
            queue_position: 0,
        }];
        if let Some(info) = info {
            overall.push(Attribute::StatusInfo(info.to_string()));
        }
        let mut attributes = vec![
            Attribute::OverallRequestStatus {
                request_id: r.id,
                attributes: overall,
            },
            Attribute::FloorRequestStatus {
                floor_id: self.floor_id,
                attributes: vec![Attribute::RequestStatus {
                    status: r.status,
                    queue_position: 0,
                }],
            },
            self.beneficiary_information(r.user_id),
            Attribute::Priority(r.priority),
        ];
        if let Some(i) = &r.info {
            attributes.push(Attribute::ParticipantProvidedInfo(i.clone()));
        }
        Attribute::FloorRequestInformation {
            request_id: r.id,
            attributes,
        }
    }

    /// A `FloorRequestStatus` about `r`.
    fn request_status(&self, echo: Echo, r: &Request, info: Option<&str>) -> Message {
        let mut m = Message::response(echo, Primitive::FloorRequestStatus)
            .with(self.floor_request_information(r, info));
        // Server-initiated: not a response, whatever the version says.
        m.responder = false;
        m
    }

    /// A `FloorStatus` for the floor: every ongoing request.
    fn floor_status(&self, echo: Echo) -> Message {
        let mut m =
            Message::response(echo, Primitive::FloorStatus).with(Attribute::FloorId(self.floor_id));
        for r in self.requests.values() {
            m.attributes.push(self.floor_request_information(r, None));
        }
        m
    }

    fn floor_status_to_subscribers(&mut self) -> Vec<Notification> {
        let subscribers: Vec<u16> = self
            .users
            .iter()
            .filter(|(_, u)| u.subscribed)
            .map(|(id, _)| *id)
            .collect();
        subscribers
            .into_iter()
            .map(|user_id| {
                let echo = self.notification_echo(user_id);
                let mut message = self.floor_status(echo);
                message.responder = false;
                Notification { user_id, message }
            })
            .collect()
    }
}

/// The status a `FloorRequestStatus` reports, for tests and owners.
pub fn reported_status(m: &Message) -> Option<(u16, RequestStatus)> {
    m.floor_request_information()
        .first()
        .and_then(|(id, info)| overall_status(info).map(|s| (*id, s)))
}
