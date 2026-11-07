//! In-memory mock relay contract tests for the ukk-internal signalling
//! envelope.
//!
//! The mock is deterministic: `send_frame` routes synchronously into the
//! destination inbox, so tests drive both endpoints without threads, sockets or
//! real network calls. The Host always registers outbound; the mock never dials
//! an endpoint.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

use ukk_protocol::Scope;
use ukk_remote::{
    validate_relay_url, CloseReason, HostId, InMemoryDeviceKeyStore, PairingClient, PairingHost,
    RelayConnection, RelayDriver, RelayError, RelayErrorCode, RelayEvent, RelayMessage, RelayState,
    RelayUrl, SessionId, StaticKeypair, DEFAULT_GRANT_TTL_MS, DEFAULT_INVITE_TTL_MS,
    MAX_PAIRING_FRAME_BYTES, MAX_SIGNALLING_FRAME_BYTES, MAX_SIGNALLING_PAYLOAD_BYTES,
    SIGNALLING_VERSION,
};

const NOW: i64 = 1_000_000;

/// Per-peer queue bound in the mock relay. Exceeding it disconnects the slow
/// peer instead of growing memory without limit.
const MAX_PEER_QUEUE_FRAMES: usize = 64;

fn host_id() -> HostId {
    HostId::new("host-1").unwrap()
}

fn relay_url() -> RelayUrl {
    RelayUrl::parse("https://relay.example.invalid").unwrap()
}

// ---------------------------------------------------------------------------
// Deterministic in-memory relay
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Core {
    next_endpoint: u64,
    next_session: u64,
    endpoints: HashMap<u64, Endpoint>,
    registrations: HashMap<HostId, Registration>,
    sessions: HashMap<SessionId, SessionRecord>,
    revoked_hosts: HashSet<HostId>,
}

struct Endpoint {
    generation: u64,
    connected: bool,
    inbox: VecDeque<Vec<u8>>,
}

#[derive(Clone)]
struct Registration {
    endpoint: u64,
    generation: u64,
}

#[derive(Clone)]
struct SessionRecord {
    host_id: HostId,
    host_endpoint: u64,
    phone_endpoint: u64,
    closed: bool,
}

struct Hub {
    core: RefCell<Core>,
}

impl Hub {
    fn new() -> Rc<Self> {
        Rc::new(Self {
            core: RefCell::new(Core::default()),
        })
    }

    fn link(self: &Rc<Self>) -> Link {
        let mut core = self.core.borrow_mut();
        core.next_endpoint += 1;
        let endpoint = core.next_endpoint;
        core.endpoints.insert(
            endpoint,
            Endpoint {
                generation: 1,
                connected: true,
                inbox: VecDeque::new(),
            },
        );
        Link {
            hub: Rc::clone(self),
            endpoint,
        }
    }
}

impl Core {
    fn deliver(&mut self, to: u64, message: RelayMessage) {
        let encoded = match message.encode() {
            Ok(encoded) => encoded,
            Err(_) => return,
        };
        if let Some(endpoint) = self.endpoints.get_mut(&to) {
            if !endpoint.connected {
                return;
            }
            if endpoint.inbox.len() >= MAX_PEER_QUEUE_FRAMES {
                // Bounded queue: a peer that never drains is disconnected
                // rather than buffered without limit.
                endpoint.connected = false;
                endpoint.inbox.clear();
                return;
            }
            endpoint.inbox.push_back(encoded);
        }
    }

    fn error(&mut self, to: u64, code: RelayErrorCode) {
        self.deliver(
            to,
            RelayMessage::Error {
                v: SIGNALLING_VERSION,
                code,
            },
        );
    }

    fn route(&mut self, from: u64, message: RelayMessage) -> Result<(), RelayError> {
        let from_generation = match self.endpoints.get(&from) {
            Some(endpoint) if endpoint.connected => endpoint.generation,
            _ => return Err(RelayError::Offline),
        };
        if message.version() != SIGNALLING_VERSION {
            return Err(RelayError::UnsupportedVersion {
                found: message.version(),
            });
        }
        match message {
            RelayMessage::Register {
                host_id,
                generation,
                ..
            } => {
                if generation != from_generation {
                    self.error(from, RelayErrorCode::Malformed);
                    return Ok(());
                }
                if self.revoked_hosts.contains(&host_id) {
                    self.error(from, RelayErrorCode::Revoked);
                    return Ok(());
                }
                if self.registrations.contains_key(&host_id) {
                    self.error(from, RelayErrorCode::DuplicateRegistration);
                    return Ok(());
                }
                self.registrations.insert(
                    host_id.clone(),
                    Registration {
                        endpoint: from,
                        generation,
                    },
                );
                self.deliver(
                    from,
                    RelayMessage::Registered {
                        v: SIGNALLING_VERSION,
                        host_id,
                        generation,
                    },
                );
                Ok(())
            }
            RelayMessage::Connect {
                host_id,
                generation,
                ..
            } => {
                if generation != from_generation {
                    self.error(from, RelayErrorCode::Malformed);
                    return Ok(());
                }
                let Some(registration) = self.registrations.get(&host_id).cloned() else {
                    self.error(from, RelayErrorCode::NotRegistered);
                    return Ok(());
                };
                let host_connected = self
                    .endpoints
                    .get(&registration.endpoint)
                    .is_some_and(|endpoint| endpoint.connected);
                if !host_connected {
                    self.registrations.remove(&host_id);
                    self.error(from, RelayErrorCode::NotRegistered);
                    return Ok(());
                }
                self.next_session += 1;
                let session_id = SessionId::parse(&format!("sess-{:016x}", self.next_session))
                    .expect("mock session id is valid");
                self.sessions.insert(
                    session_id.clone(),
                    SessionRecord {
                        host_id: host_id.clone(),
                        host_endpoint: registration.endpoint,
                        phone_endpoint: from,
                        closed: false,
                    },
                );
                self.deliver(
                    registration.endpoint,
                    RelayMessage::Route {
                        v: SIGNALLING_VERSION,
                        generation: registration.generation,
                        session_id: session_id.clone(),
                    },
                );
                self.deliver(
                    from,
                    RelayMessage::Session {
                        v: SIGNALLING_VERSION,
                        host_id,
                        generation,
                        session_id,
                    },
                );
                Ok(())
            }
            RelayMessage::Frame {
                session_id, frame, ..
            } => {
                let Some(record) = self.sessions.get(&session_id).cloned() else {
                    self.error(from, RelayErrorCode::UnknownRoute);
                    return Ok(());
                };
                if record.closed {
                    self.error(from, RelayErrorCode::SessionClosed);
                    return Ok(());
                }
                let to = if from == record.host_endpoint {
                    record.phone_endpoint
                } else if from == record.phone_endpoint {
                    record.host_endpoint
                } else {
                    self.error(from, RelayErrorCode::UnknownRoute);
                    return Ok(());
                };
                let connected = self
                    .endpoints
                    .get(&to)
                    .is_some_and(|endpoint| endpoint.connected);
                if !connected {
                    self.error(from, RelayErrorCode::Offline);
                    return Ok(());
                }
                self.deliver(
                    to,
                    RelayMessage::Frame {
                        v: SIGNALLING_VERSION,
                        session_id,
                        frame,
                    },
                );
                Ok(())
            }
            RelayMessage::Close {
                session_id, reason, ..
            } => {
                let Some(record) = self.sessions.get_mut(&session_id) else {
                    self.error(from, RelayErrorCode::UnknownRoute);
                    return Ok(());
                };
                if from != record.host_endpoint && from != record.phone_endpoint {
                    self.error(from, RelayErrorCode::UnknownRoute);
                    return Ok(());
                }
                let to = if from == record.host_endpoint {
                    record.phone_endpoint
                } else {
                    record.host_endpoint
                };
                record.closed = true;
                self.deliver(
                    to,
                    RelayMessage::Close {
                        v: SIGNALLING_VERSION,
                        session_id,
                        reason,
                    },
                );
                Ok(())
            }
            // These are relay-originated messages; a peer must never send them.
            RelayMessage::Registered { .. }
            | RelayMessage::Session { .. }
            | RelayMessage::Route { .. }
            | RelayMessage::Error { .. } => {
                self.error(from, RelayErrorCode::Malformed);
                Ok(())
            }
        }
    }

    fn disconnect(&mut self, endpoint: u64) {
        let Some(peer) = self.endpoints.get_mut(&endpoint) else {
            return;
        };
        if !peer.connected {
            return;
        }
        peer.connected = false;
        peer.inbox.clear();

        let registered_host = self
            .registrations
            .iter()
            .find_map(|(host_id, registration)| {
                (registration.endpoint == endpoint).then(|| host_id.clone())
            });
        if let Some(host_id) = registered_host {
            self.registrations.remove(&host_id);
        }

        let mut closed = Vec::new();
        for (session_id, record) in &self.sessions {
            if record.closed {
                continue;
            }
            if record.host_endpoint == endpoint {
                closed.push((record.phone_endpoint, session_id.clone()));
            } else if record.phone_endpoint == endpoint {
                closed.push((record.host_endpoint, session_id.clone()));
            }
        }
        for (to, session_id) in closed {
            if let Some(record) = self.sessions.get_mut(&session_id) {
                record.closed = true;
            }
            // A Host reconnect removes its sessions: an old route is never
            // resumable.
            if self
                .sessions
                .get(&session_id)
                .is_some_and(|record| record.host_endpoint == endpoint)
            {
                self.sessions.remove(&session_id);
            }
            self.deliver(
                to,
                RelayMessage::Close {
                    v: SIGNALLING_VERSION,
                    session_id,
                    reason: CloseReason::Offline,
                },
            );
        }
    }

    fn revoke(&mut self, host_id: &HostId) {
        self.revoked_hosts.insert(host_id.clone());
        let host_endpoint = self
            .registrations
            .remove(host_id)
            .map(|registration| registration.endpoint);
        let mut deliveries = Vec::new();
        for (session_id, record) in self.sessions.iter_mut() {
            if record.host_id == *host_id && !record.closed {
                record.closed = true;
                deliveries.push((
                    record.host_endpoint,
                    RelayMessage::Close {
                        v: SIGNALLING_VERSION,
                        session_id: session_id.clone(),
                        reason: CloseReason::Revoked,
                    },
                ));
                deliveries.push((
                    record.phone_endpoint,
                    RelayMessage::Close {
                        v: SIGNALLING_VERSION,
                        session_id: session_id.clone(),
                        reason: CloseReason::Revoked,
                    },
                ));
            }
        }
        if let Some(endpoint) = host_endpoint {
            deliveries.push((
                endpoint,
                RelayMessage::Error {
                    v: SIGNALLING_VERSION,
                    code: RelayErrorCode::Revoked,
                },
            ));
        }
        for (to, message) in deliveries {
            self.deliver(to, message);
        }
    }
}

#[derive(Clone)]
struct Link {
    hub: Rc<Hub>,
    endpoint: u64,
}

impl Link {
    fn send(&mut self, message: RelayMessage) -> Result<(), RelayError> {
        let encoded = message.encode()?;
        self.send_frame(&encoded)
    }

    fn recv(&mut self) -> RelayMessage {
        let bytes = self
            .recv_frame(MAX_SIGNALLING_FRAME_BYTES)
            .expect("relay delivered a frame");
        RelayMessage::decode(&bytes).expect("relay frames are valid envelopes")
    }

    fn disconnect(&mut self) {
        self.hub.core.borrow_mut().disconnect(self.endpoint);
    }

    fn reconnect(&mut self) {
        let mut core = self.hub.core.borrow_mut();
        core.disconnect(self.endpoint);
        if let Some(endpoint) = core.endpoints.get_mut(&self.endpoint) {
            endpoint.generation += 1;
            endpoint.connected = true;
        }
    }

    fn revoke(&self, host_id: &HostId) {
        self.hub.core.borrow_mut().revoke(host_id);
    }
}

impl RelayConnection for Link {
    type Error = RelayError;

    fn generation(&self) -> u64 {
        self.hub
            .core
            .borrow()
            .endpoints
            .get(&self.endpoint)
            .map_or(0, |endpoint| endpoint.generation)
    }

    fn send_frame(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        if frame.len() > MAX_SIGNALLING_FRAME_BYTES {
            return Err(RelayError::FrameTooLarge {
                max: MAX_SIGNALLING_FRAME_BYTES,
            });
        }
        let message = RelayMessage::decode(frame)?;
        let mut core = self.hub.core.borrow_mut();
        if !core
            .endpoints
            .get(&self.endpoint)
            .is_some_and(|endpoint| endpoint.connected)
        {
            return Err(RelayError::Offline);
        }
        core.route(self.endpoint, message)
    }

    fn recv_frame(&mut self, max_bytes: usize) -> Result<Vec<u8>, Self::Error> {
        let mut core = self.hub.core.borrow_mut();
        let Some(endpoint) = core.endpoints.get_mut(&self.endpoint) else {
            return Err(RelayError::Offline);
        };
        match endpoint.inbox.front() {
            None => Err(RelayError::Offline),
            Some(frame) if frame.len() > max_bytes => {
                Err(RelayError::FrameTooLarge { max: max_bytes })
            }
            Some(_) => Ok(endpoint.inbox.pop_front().expect("front exists")),
        }
    }
}

fn send_payload(driver: &RelayDriver, link: &mut Link, payload: &[u8]) -> RelayMessage {
    let message = driver.payload_frame(payload).unwrap();
    link.send(message.clone()).unwrap();
    message
}

fn route_phone(
    host_link: &mut Link,
    host: &mut RelayDriver,
    phone_link: &mut Link,
    phone: &mut RelayDriver,
) -> SessionId {
    let connect = phone.begin(&*phone_link).unwrap();
    phone_link.send(connect).unwrap();
    let session = phone_link.recv();
    let RelayEvent::SessionOpened { session_id } = phone.handle(session).unwrap() else {
        panic!("phone must receive a session");
    };
    let route = host_link.recv();
    assert_eq!(
        host.handle(route).unwrap(),
        RelayEvent::IncomingSession {
            session_id: session_id.clone()
        }
    );
    session_id
}

fn register_host(host_link: &mut Link, host: &mut RelayDriver) {
    let register = host.begin(&*host_link).unwrap();
    host_link.send(register).unwrap();
    let registered = host_link.recv();
    assert!(matches!(
        host.handle(registered).unwrap(),
        RelayEvent::RegisterAccepted { .. }
    ));
}

// ---------------------------------------------------------------------------
// Contract tests
// ---------------------------------------------------------------------------

#[test]
fn registers_outbound_and_carries_a_full_pairing_exchange() {
    let hub = Hub::new();
    let mut host_link = hub.link();
    let mut relay_host = RelayDriver::host(host_id());
    register_host(&mut host_link, &mut relay_host);
    assert!(matches!(
        relay_host.state(),
        RelayState::Registered { generation: 1 }
    ));

    let mut pairing_host = PairingHost::new(host_id(), StaticKeypair::generate().unwrap());
    let invite = pairing_host
        .create_invite(relay_url(), NOW, DEFAULT_INVITE_TTL_MS)
        .unwrap();
    let client = PairingClient::new(
        invite,
        Box::new(InMemoryDeviceKeyStore::new()),
        &[Scope::SessionObserve],
    )
    .unwrap();

    let mut phone_link = hub.link();
    let mut phone = RelayDriver::phone(host_id());
    let session_id = route_phone(&mut host_link, &mut relay_host, &mut phone_link, &mut phone);

    // Opaque pairing request routed phone -> Host.
    let request = client.request_frame().unwrap();
    assert!(request.len() <= MAX_PAIRING_FRAME_BYTES);
    send_payload(&phone, &mut phone_link, &request);
    let RelayEvent::Frame {
        session_id: received_on,
        frame,
    } = relay_host.handle(host_link.recv()).unwrap()
    else {
        panic!("Host must receive the request frame");
    };
    assert_eq!(received_on, session_id);
    assert_eq!(frame.as_bytes(), request);

    // Hello phone -> Host, then the Host response back.
    let (hello, handshake) = client.start(NOW + 10).unwrap();
    send_payload(&phone, &mut phone_link, &hello);
    let RelayEvent::Frame {
        frame: hello_frame, ..
    } = relay_host.handle(host_link.recv()).unwrap()
    else {
        panic!("Host must receive the hello frame");
    };
    let attempt = pairing_host
        .accept(&request, hello_frame.as_bytes(), NOW + 20)
        .unwrap();
    let pending_client = handshake
        .finish(attempt.response_frame(), NOW + 30)
        .unwrap();
    send_payload(&relay_host, &mut host_link, attempt.response_frame());
    let RelayEvent::Frame {
        frame: response, ..
    } = phone.handle(phone_link.recv()).unwrap()
    else {
        panic!("phone must receive the response frame");
    };
    assert_eq!(response.as_bytes(), attempt.response_frame());

    // Confirmation and grant, still over the mock relay only.
    assert_eq!(
        pending_client.confirmation_code(),
        attempt.confirmation_code()
    );
    let code = attempt.confirmation_code().digits();
    let confirmed = attempt.confirm(&code).unwrap();
    let approved = pairing_host
        .approve(
            confirmed,
            &[Scope::SessionObserve],
            DEFAULT_GRANT_TTL_MS,
            NOW + 40,
        )
        .unwrap();
    send_payload(&relay_host, &mut host_link, approved.frame());
    let RelayEvent::Frame { frame: grant, .. } = phone.handle(phone_link.recv()).unwrap() else {
        panic!("phone must receive the grant frame");
    };
    let accepted = pending_client
        .await_approval(grant.as_bytes(), NOW + 50)
        .unwrap();
    send_payload(&phone, &mut phone_link, accepted.ack_frame());
    let RelayEvent::Frame { frame: ack, .. } = relay_host.handle(host_link.recv()).unwrap() else {
        panic!("Host must receive the ack frame");
    };
    pairing_host
        .complete(approved, ack.as_bytes(), NOW + 50)
        .unwrap();
    assert!(pairing_host.trust().status(client.device_id()).is_some());
}

#[test]
fn duplicate_registration_fails_closed_and_keeps_the_first_connection() {
    let hub = Hub::new();
    let mut first_link = hub.link();
    let mut first = RelayDriver::host(host_id());
    register_host(&mut first_link, &mut first);

    let mut second_link = hub.link();
    let mut second = RelayDriver::host(host_id());
    let register = second.begin(&second_link).unwrap();
    second_link.send(register).unwrap();
    let error = second_link.recv();
    assert_eq!(
        error,
        RelayMessage::Error {
            v: SIGNALLING_VERSION,
            code: RelayErrorCode::DuplicateRegistration,
        }
    );
    assert_eq!(second.handle(error), Err(RelayError::DuplicateRegistration));
    assert_eq!(second.state(), &RelayState::Closed);

    // The first registration still works; the duplicate did not evict it.
    let mut phone_link = hub.link();
    let mut phone = RelayDriver::phone(host_id());
    route_phone(&mut first_link, &mut first, &mut phone_link, &mut phone);
}

#[test]
fn offline_unknown_route_and_resume_field_fail_closed() {
    let hub = Hub::new();

    // No Host registered yet -> typed NotRegistered and a closed phone driver.
    let mut phone_link = hub.link();
    let mut phone = RelayDriver::phone(host_id());
    let connect = phone.begin(&phone_link).unwrap();
    phone_link.send(connect).unwrap();
    let error = phone_link.recv();
    assert_eq!(
        error,
        RelayMessage::Error {
            v: SIGNALLING_VERSION,
            code: RelayErrorCode::NotRegistered,
        }
    );
    assert_eq!(phone.handle(error), Err(RelayError::NotRegistered));

    // Resume is not part of envelope v1: a frame carrying it is rejected as
    // an unknown field (and the driver closes) before the relay sees it.
    let resumed_link = hub.link();
    let mut resumed = RelayDriver::phone(host_id());
    resumed.begin(&resumed_link).unwrap();
    let resume = br#"{"type":"connect","body":{"v":1,"host_id":"host-1","generation":1,"resume_session":"sess-00000000000000ff"}}"#;
    assert_eq!(RelayMessage::decode(resume), Err(RelayError::Malformed));
    assert_eq!(resumed.handle_frame(resume), Err(RelayError::Malformed));
    assert_eq!(resumed.state(), &RelayState::Closed);

    // A frame for a session the relay does not know is refused.
    let mut host_link = hub.link();
    let mut host = RelayDriver::host(host_id());
    register_host(&mut host_link, &mut host);
    let mut routed_link = hub.link();
    let mut routed = RelayDriver::phone(host_id());
    let session_id = route_phone(&mut host_link, &mut host, &mut routed_link, &mut routed);
    let bogus = SessionId::parse("sess-00000000000000aa").unwrap();
    assert_ne!(bogus, session_id);
    routed_link
        .send(RelayMessage::Frame {
            v: SIGNALLING_VERSION,
            session_id: bogus,
            frame: ukk_remote::OpaqueFrame::new(b"lost").unwrap(),
        })
        .unwrap();
    let error = routed_link.recv();
    assert_eq!(
        error,
        RelayMessage::Error {
            v: SIGNALLING_VERSION,
            code: RelayErrorCode::UnknownRoute,
        }
    );
    assert_eq!(routed.handle(error), Err(RelayError::UnknownRoute));
}

#[test]
fn reconnect_advances_generation_and_old_routes_are_not_resumable() {
    let hub = Hub::new();
    let mut host_link = hub.link();
    let mut host = RelayDriver::host(host_id());
    register_host(&mut host_link, &mut host);

    let mut phone_link = hub.link();
    let mut phone = RelayDriver::phone(host_id());
    let old_session = route_phone(&mut host_link, &mut host, &mut phone_link, &mut phone);
    send_payload(&phone, &mut phone_link, b"before reconnect");
    assert!(matches!(
        host.handle(host_link.recv()).unwrap(),
        RelayEvent::Frame { .. }
    ));

    // A generation already used by this driver is never reusable; a reconnect
    // must start a fresh driver with the higher generation.
    assert_eq!(host.begin(&host_link), Err(RelayError::AlreadyConnected));
    assert_eq!(host.state(), &RelayState::Closed);

    // Host link drops: the phone sees a typed offline close.
    host_link.disconnect();
    let close = phone_link.recv();
    assert!(matches!(
        phone.handle(close).unwrap(),
        RelayEvent::Closed {
            reason: CloseReason::Offline,
            ..
        }
    ));
    assert_eq!(phone.state(), &RelayState::Closed);

    // Reconnect with a strictly higher generation.
    host_link.reconnect();
    assert_eq!(host_link.generation(), 2);
    let mut restarted_host = RelayDriver::host(host_id());
    register_host(&mut host_link, &mut restarted_host);

    let mut new_phone_link = hub.link();
    let mut new_phone = RelayDriver::phone(host_id());
    let new_session = route_phone(
        &mut host_link,
        &mut restarted_host,
        &mut new_phone_link,
        &mut new_phone,
    );
    assert_ne!(old_session, new_session);

    // The old route no longer exists on the relay.
    new_phone_link
        .send(RelayMessage::Frame {
            v: SIGNALLING_VERSION,
            session_id: old_session,
            frame: ukk_remote::OpaqueFrame::new(b"stale").unwrap(),
        })
        .unwrap();
    assert_eq!(
        new_phone_link.recv(),
        RelayMessage::Error {
            v: SIGNALLING_VERSION,
            code: RelayErrorCode::UnknownRoute,
        }
    );
}

#[test]
fn oversized_frames_and_payloads_are_refused_before_buffering() {
    let hub = Hub::new();
    let mut link = hub.link();

    let oversized = vec![b'0'; MAX_SIGNALLING_FRAME_BYTES + 1];
    assert_eq!(
        link.send_frame(&oversized),
        Err(RelayError::FrameTooLarge {
            max: MAX_SIGNALLING_FRAME_BYTES
        })
    );
    assert_eq!(
        RelayMessage::decode(&oversized),
        Err(RelayError::FrameTooLarge {
            max: MAX_SIGNALLING_FRAME_BYTES
        })
    );
    assert_eq!(
        ukk_remote::OpaqueFrame::new(&vec![0_u8; MAX_SIGNALLING_PAYLOAD_BYTES + 1]),
        Err(RelayError::PayloadTooLarge {
            max: MAX_SIGNALLING_PAYLOAD_BYTES
        })
    );

    // The receive bound is enforced without popping the frame.
    let mut host_link = hub.link();
    let mut host = RelayDriver::host(host_id());
    register_host(&mut host_link, &mut host);
    let mut phone_link = hub.link();
    let mut phone = RelayDriver::phone(host_id());
    route_phone(&mut host_link, &mut host, &mut phone_link, &mut phone);
    send_payload(&phone, &mut phone_link, b"bounded");
    assert_eq!(
        host_link.recv_frame(1),
        Err(RelayError::FrameTooLarge { max: 1 })
    );
    assert!(matches!(
        host.handle(host_link.recv()).unwrap(),
        RelayEvent::Frame { .. }
    ));
}

#[test]
fn revocation_and_session_close_fail_closed() {
    let hub = Hub::new();
    let mut host_link = hub.link();
    let mut host = RelayDriver::host(host_id());
    register_host(&mut host_link, &mut host);

    let mut phone_link = hub.link();
    let mut phone = RelayDriver::phone(host_id());
    let session_id = route_phone(&mut host_link, &mut host, &mut phone_link, &mut phone);

    host_link.revoke(&host_id());
    let phone_close = phone_link.recv();
    assert_eq!(
        phone_close,
        RelayMessage::Close {
            v: SIGNALLING_VERSION,
            session_id: session_id.clone(),
            reason: CloseReason::Revoked,
        }
    );
    assert!(matches!(
        phone.handle(phone_close).unwrap(),
        RelayEvent::Closed {
            reason: CloseReason::Revoked,
            ..
        }
    ));
    // Host first observes the route close, then the fatal error.
    assert!(matches!(
        host.handle(host_link.recv()).unwrap(),
        RelayEvent::Closed {
            reason: CloseReason::Revoked,
            ..
        }
    ));
    let host_error = host_link.recv();
    assert_eq!(
        host_error,
        RelayMessage::Error {
            v: SIGNALLING_VERSION,
            code: RelayErrorCode::Revoked,
        }
    );
    assert_eq!(host.handle(host_error), Err(RelayError::Revoked));
    assert_eq!(host.state(), &RelayState::Closed);

    // A revoked Host cannot register again with the same relay.
    let mut retry_link = hub.link();
    let mut retry = RelayDriver::host(host_id());
    let register = retry.begin(&retry_link).unwrap();
    retry_link.send(register).unwrap();
    let error = retry_link.recv();
    assert_eq!(
        error,
        RelayMessage::Error {
            v: SIGNALLING_VERSION,
            code: RelayErrorCode::Revoked,
        }
    );
    assert_eq!(retry.handle(error), Err(RelayError::Revoked));

    // Ordinary close leaves the session terminal for further frames.
    let hub = Hub::new();
    let mut host_link = hub.link();
    let mut host = RelayDriver::host(host_id());
    register_host(&mut host_link, &mut host);
    let mut phone_link = hub.link();
    let mut phone = RelayDriver::phone(host_id());
    let session_id = route_phone(&mut host_link, &mut host, &mut phone_link, &mut phone);
    let close = phone.close_frame(CloseReason::ProtocolError).unwrap();
    phone_link.send(close).unwrap();
    assert!(matches!(
        host.handle(host_link.recv()).unwrap(),
        RelayEvent::Closed {
            reason: CloseReason::ProtocolError,
            ..
        }
    ));
    host_link
        .send(RelayMessage::Frame {
            v: SIGNALLING_VERSION,
            session_id,
            frame: ukk_remote::OpaqueFrame::new(b"too late").unwrap(),
        })
        .unwrap();
    assert_eq!(
        host_link.recv(),
        RelayMessage::Error {
            v: SIGNALLING_VERSION,
            code: RelayErrorCode::SessionClosed,
        }
    );
}

#[test]
fn relay_queues_are_bounded_and_overflow_fails_closed() {
    let hub = Hub::new();
    let mut host_link = hub.link();
    let mut host = RelayDriver::host(host_id());
    register_host(&mut host_link, &mut host);
    let mut phone_link = hub.link();
    let mut phone = RelayDriver::phone(host_id());
    route_phone(&mut host_link, &mut host, &mut phone_link, &mut phone);

    // Fill the Host inbox without reading, then push one more.
    for index in 0..=MAX_PEER_QUEUE_FRAMES {
        send_payload(&phone, &mut phone_link, format!("frame-{index}").as_bytes());
    }
    // The overflow disconnected the slow peer and cleared its queue.
    assert_eq!(
        host_link.recv_frame(MAX_SIGNALLING_FRAME_BYTES),
        Err(RelayError::Offline)
    );

    // The phone learns on its next frame that the route is gone.
    send_payload(&phone, &mut phone_link, b"after overflow");
    let error = phone_link.recv();
    assert_eq!(
        error,
        RelayMessage::Error {
            v: SIGNALLING_VERSION,
            code: RelayErrorCode::Offline,
        }
    );
    assert_eq!(phone.handle(error), Err(RelayError::Offline));
    assert_eq!(phone.state(), &RelayState::Closed);
}

#[test]
fn relay_url_validation_stays_https_or_wss() {
    for raw in [
        "https://relay.example.invalid",
        "wss://relay.example.invalid/path",
    ] {
        let parsed = validate_relay_url(raw).unwrap();
        assert!(matches!(parsed.scheme(), "https" | "wss"));
    }
    for raw in [
        "http://relay.example.invalid",
        "ws://relay.example.invalid",
        "file:///tmp/relay",
        "https://relay.example.invalid:0",
        "relay.example.invalid",
    ] {
        assert!(validate_relay_url(raw).is_err(), "{raw} must be refused");
    }
}
