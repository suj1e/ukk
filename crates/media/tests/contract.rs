//! Mock-peer contract tests for the M1 media/signalling boundary.
//!
//! There is no socket, camera, screen, TURN server or WebRTC stack here. Two
//! in-memory peers exchange encoded signalling bytes exactly as a future
//! transport adapter would.

use ukk_media::{
    CodecDescriptor, Credential, FailureReason, GrantAuthority, GrantError, IceCandidate,
    IceServer, IceTransport, IceUrl, InputDenial, MediaCapabilities, MediaSession, MediaSessionId,
    MediaState, PauseReason, Sdp, SignallingError, SignallingMessage, StopReason,
    TransportCapability, MAX_SIGNALLING_MESSAGE_BYTES,
};
use ukk_protocol::{DeviceId, DomainId};

const NOW: i64 = 1_000_000;
const SESSION_TTL: i64 = 60_000;

fn media_id() -> MediaSessionId {
    MediaSessionId::parse("media-000000000001").unwrap()
}

fn other_media_id() -> MediaSessionId {
    MediaSessionId::parse("media-000000000002").unwrap()
}

fn device(name: &str) -> DeviceId {
    DeviceId::new(format!("device-{name}")).unwrap()
}

fn session_id() -> DomainId {
    DomainId::new("session-1").unwrap()
}

fn other_session_id() -> DomainId {
    DomainId::new("session-2").unwrap()
}

fn sdp() -> Sdp {
    Sdp::new("v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\na=fingerprint:sha-256 secret\r\n")
        .unwrap()
}

fn codec(name: &str, clock_rate: u32, channels: u8) -> CodecDescriptor {
    CodecDescriptor::new(name, clock_rate, channels).unwrap()
}

fn udp() -> TransportCapability {
    TransportCapability::new(IceTransport::Udp, true).unwrap()
}

fn tcp() -> TransportCapability {
    TransportCapability::new(IceTransport::Tcp, true).unwrap()
}

fn turn_server() -> IceServer {
    IceServer::new(
        vec![IceUrl::parse("turn:turn.example.invalid:3478?transport=udp").unwrap()],
        Some(Credential::new("turn-user").unwrap()),
        Some(Credential::new("turn-secret-value").unwrap()),
    )
    .unwrap()
}

fn offer_capabilities() -> MediaCapabilities {
    MediaCapabilities::new(
        vec![codec("h264", 90_000, 0), codec("opus", 48_000, 2)],
        vec![udp(), tcp()],
        vec![turn_server()],
    )
    .unwrap()
}

fn viewer_capabilities() -> MediaCapabilities {
    MediaCapabilities::new(
        vec![codec("h264", 90_000, 0), codec("opus", 48_000, 2)],
        vec![udp()],
        Vec::new(),
    )
    .unwrap()
}

fn candidate() -> IceCandidate {
    IceCandidate::new("candidate:1 1 udp 1 192.0.2.10 5000 typ host").unwrap()
}

fn connected_host() -> ukk_media::HostSession {
    let mut host =
        ukk_media::HostSession::new(media_id(), NOW + SESSION_TTL, offer_capabilities(), sdp())
            .unwrap();
    let mut viewer = MediaSession::viewer(media_id(), viewer_capabilities(), sdp()).unwrap();
    let offer = host.start(NOW).unwrap();
    let answer = viewer.handle(&offer, NOW).unwrap().expect("answer");
    host.handle(&answer, NOW).unwrap();
    assert_eq!(host.state(), &MediaState::Connected);
    host
}

#[test]
fn offer_answer_and_trickle_ice_follow_the_defined_order() {
    let mut host =
        ukk_media::HostSession::new(media_id(), NOW + SESSION_TTL, offer_capabilities(), sdp())
            .unwrap();
    let mut viewer = MediaSession::viewer(media_id(), viewer_capabilities(), sdp()).unwrap();

    assert_eq!(host.state(), &MediaState::Idle);
    let offer = host.start(NOW).unwrap();
    assert_eq!(host.state(), &MediaState::OfferSent);
    assert_eq!(offer.kind(), "offer");

    let wire = offer.encode().unwrap();
    let decoded = SignallingMessage::decode(&wire).unwrap();
    assert_eq!(decoded, offer);

    // Host may trickle a candidate before the answer arrives.
    let early = host.add_ice_candidate(NOW, candidate()).unwrap();
    let answer = viewer.handle(&decoded, NOW).unwrap().expect("answer");
    assert_eq!(viewer.state(), &MediaState::Connected);
    match &answer {
        SignallingMessage::Answer { capabilities, .. } => {
            assert!(capabilities.ice_servers.is_empty());
        }
        other => panic!("expected answer, got {other}"),
    }
    let answer_bytes = answer.encode().unwrap();
    let answer = SignallingMessage::decode(&answer_bytes).unwrap();

    viewer.handle(&early, NOW).unwrap();
    host.handle(&answer, NOW).unwrap();
    assert_eq!(host.state(), &MediaState::Connected);
    assert!(host.media().negotiated().is_some());

    let host_candidate = host.add_ice_candidate(NOW, candidate()).unwrap();
    viewer.handle(&host_candidate, NOW).unwrap();
    let viewer_candidate = viewer.add_ice_candidate(NOW, candidate()).unwrap();
    host.handle(&viewer_candidate, NOW).unwrap();

    let host_done = host.ice_complete(NOW).unwrap();
    viewer.handle(&host_done, NOW).unwrap();
    let viewer_done = viewer.ice_complete(NOW).unwrap();
    host.handle(&viewer_done, NOW).unwrap();
    assert_eq!(viewer.state(), &MediaState::Connected);
}

#[test]
fn wrong_order_session_and_role_are_rejected() {
    let host_session =
        ukk_media::HostSession::new(media_id(), NOW + SESSION_TTL, offer_capabilities(), sdp())
            .unwrap();

    // A viewer has no offer of its own.
    let mut viewer = MediaSession::viewer(media_id(), viewer_capabilities(), sdp()).unwrap();
    assert_eq!(
        viewer.start(NOW),
        Err(SignallingError::WrongRole(
            "only the capture-side Host starts an offer"
        ))
    );
    let mut early = MediaSession::viewer(media_id(), viewer_capabilities(), sdp()).unwrap();
    assert_eq!(
        early.handle(
            &SignallingMessage::IceCandidate {
                v: 1,
                media_session_id: media_id(),
                candidate: candidate(),
            },
            NOW
        ),
        Err(SignallingError::UnexpectedMessage("idle"))
    );

    // A Host does not accept an offer.
    let mut host = host_session;
    let offer_from_viewer = SignallingMessage::Offer {
        v: 1,
        media_session_id: media_id(),
        expires_at_ms: NOW + SESSION_TTL,
        capabilities: offer_capabilities(),
        sdp: sdp(),
    };
    assert_eq!(
        host.handle(&offer_from_viewer, NOW),
        Err(SignallingError::WrongRole(
            "a Host does not accept an offer from a viewer"
        ))
    );

    // Wrong media session id is refused before any state changes.
    let mut wrong_viewer =
        MediaSession::viewer(other_media_id(), viewer_capabilities(), sdp()).unwrap();
    assert_eq!(
        wrong_viewer.handle(&offer_from_viewer, NOW),
        Err(SignallingError::WrongSession)
    );

    // Answer expiry must equal the offer expiry.
    let mut host =
        ukk_media::HostSession::new(media_id(), NOW + SESSION_TTL, offer_capabilities(), sdp())
            .unwrap();
    let mut viewer = MediaSession::viewer(media_id(), viewer_capabilities(), sdp()).unwrap();
    let offer = host.start(NOW).unwrap();
    let answer = viewer.handle(&offer, NOW).unwrap().unwrap();
    let wrong_expiry = SignallingMessage::Answer {
        v: 1,
        media_session_id: media_id(),
        expires_at_ms: NOW + SESSION_TTL + 1,
        capabilities: viewer_capabilities(),
        sdp: sdp(),
    };
    assert_eq!(
        host.handle(&wrong_expiry, NOW),
        Err(SignallingError::ExpiryMismatch)
    );

    // An answer cannot widen the offered codec set.
    let mut extra_codec = viewer_capabilities();
    extra_codec.codecs.push(codec("av1", 90_000, 0));
    let widened = SignallingMessage::Answer {
        v: 1,
        media_session_id: media_id(),
        expires_at_ms: NOW + SESSION_TTL,
        capabilities: extra_codec,
        sdp: sdp(),
    };
    assert_eq!(
        host.handle(&widened, NOW),
        Err(SignallingError::InvalidAnswer(
            "answer codecs/transports are not a subset of the offer"
        ))
    );
    host.handle(&answer, NOW).unwrap();

    // Viewer-side pause is not a Host command.
    assert_eq!(
        viewer.pause(NOW, PauseReason::UserPaused),
        Err(SignallingError::WrongRole(
            "only the capture-side Host pauses media"
        ))
    );
}

#[test]
fn malformed_oversized_and_media_frame_messages_fail_closed() {
    assert_eq!(
        SignallingMessage::decode(&vec![b'{'; MAX_SIGNALLING_MESSAGE_BYTES + 1]),
        Err(SignallingError::Oversized {
            actual: MAX_SIGNALLING_MESSAGE_BYTES + 1,
            max: MAX_SIGNALLING_MESSAGE_BYTES,
        })
    );
    assert_eq!(
        SignallingMessage::decode(br#"["offer",{"v":1}]"#),
        Err(SignallingError::Malformed)
    );
    assert_eq!(
        SignallingMessage::decode(br#"{"type":"stop","body":{"v":1,"media_session_id":"media-000000000001","reason":"user_closed","frame":"AAAA"}}"#),
        Err(SignallingError::Malformed)
    );
    assert_eq!(
        SignallingMessage::decode(br#"{"type":"media_frame","body":{"v":1}}"#),
        Err(SignallingError::Malformed)
    );
    // A relay `frame` envelope cannot be reinterpreted as media signalling.
    assert_eq!(
        SignallingMessage::decode(br#"{"type":"frame","body":{"v":1,"session_id":"sess-0000000000000001","frame":"AAAA"}}"#),
        Err(SignallingError::Malformed)
    );
    // Nor can a durable event envelope type produce a media frame.
    assert_eq!(
        SignallingMessage::decode(br#"{"type":"event","body":{"v":1,"frame":"AAAA"}}"#),
        Err(SignallingError::Malformed)
    );

    let mut encoded_offer = SignallingMessage::Offer {
        v: 1,
        media_session_id: media_id(),
        expires_at_ms: NOW + SESSION_TTL,
        capabilities: offer_capabilities(),
        sdp: sdp(),
    }
    .encode()
    .unwrap();
    encoded_offer.push(b'x');
    assert_eq!(
        SignallingMessage::decode(&encoded_offer),
        Err(SignallingError::Malformed)
    );

    let mut value: serde_json::Value = serde_json::from_slice(
        &SignallingMessage::Offer {
            v: 1,
            media_session_id: media_id(),
            expires_at_ms: NOW + SESSION_TTL,
            capabilities: offer_capabilities(),
            sdp: sdp(),
        }
        .encode()
        .unwrap(),
    )
    .unwrap();
    value["body"]["v"] = serde_json::json!(2);
    assert_eq!(
        SignallingMessage::decode(value.to_string().as_bytes()),
        Err(SignallingError::UnsupportedVersion { found: 2 })
    );

    assert!(Sdp::new(format!("v=0\r\n{}", "a".repeat(20_000))).is_err());
    assert!(IceCandidate::new("candidate:1\r\nmalformed").is_err());
    assert!(IceCandidate::new("a".repeat(2_000)).is_err());
}

#[test]
fn turn_urls_and_credentials_are_typed_and_redacted() {
    for valid in [
        "stun:stun.example.invalid:3478",
        "turn:turn.example.invalid:3478?transport=udp",
        "turns:turn.example.invalid:5349?transport=tcp",
    ] {
        assert!(IceUrl::parse(valid).is_ok());
    }
    for invalid in [
        "http://turn.example.invalid",
        "ws://turn.example.invalid",
        "turn://turn.example.invalid",
        "turn:user@turn.example.invalid",
        "stun:stun.example.invalid?transport=udp",
    ] {
        assert!(IceUrl::parse(invalid).is_err(), "{invalid}");
    }

    let server = turn_server();
    let server_debug = format!("{server:?}");
    assert!(!server_debug.contains("turn-user"));
    assert!(!server_debug.contains("turn-secret-value"));

    let mut host =
        ukk_media::HostSession::new(media_id(), NOW + SESSION_TTL, offer_capabilities(), sdp())
            .unwrap();
    let offer = host.start(NOW).unwrap();
    let offer_debug = format!("{offer:?}");
    let offer_display = format!("{offer}");
    assert!(!offer_debug.contains("turn-secret-value"));
    assert!(!offer_display.contains("turn-secret-value"));
    assert!(!offer_debug.contains("192.0.2.10"));
    assert!(!format!("{:?}", sdp()).contains("fingerprint"));

    // Only the explicit transport accessor exposes the secret.
    assert_eq!(
        turn_server()
            .credential
            .expect("credential")
            .expose_for_transport(),
        "turn-secret-value"
    );
}

#[test]
fn no_media_payload_field_exists_on_the_control_plane() {
    let offer = SignallingMessage::Offer {
        v: 1,
        media_session_id: media_id(),
        expires_at_ms: NOW + SESSION_TTL,
        capabilities: offer_capabilities(),
        sdp: sdp(),
    }
    .encode()
    .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&offer).unwrap();
    let top = value.as_object().unwrap();
    assert_eq!(top.len(), 2);
    assert!(top.contains_key("type"));
    assert!(top.contains_key("body"));
    let body = top["body"].as_object().unwrap();
    for forbidden in [
        "frame", "frames", "sample", "samples", "pixel", "pixels", "media",
    ] {
        assert!(!body.contains_key(forbidden));
    }
}

#[test]
fn view_and_control_grants_are_separate_and_control_requires_view() {
    let mut authority = GrantAuthority::new();
    let view_only_device = device("view-only");

    assert_eq!(
        authority.grant_control(&ukk_media::GrantId::new_random(), NOW, 1_000),
        Err(GrantError::ViewGrantMissing)
    );

    let view = authority
        .grant_view(
            view_only_device.clone(),
            session_id(),
            media_id(),
            NOW,
            30_000,
        )
        .unwrap();
    assert_eq!(
        authority.authorize_control(&view_only_device, &session_id(), &media_id(), NOW),
        Err(GrantError::ControlGrantMissing)
    );
    assert_eq!(
        authority
            .authorize_view(&view_only_device, &session_id(), &media_id(), NOW)
            .map(|g| g.id.clone()),
        Ok(view.id.clone())
    );

    // A control grant for another session does not match this media session.
    let other_view = authority
        .grant_view(
            view_only_device.clone(),
            other_session_id(),
            media_id(),
            NOW,
            30_000,
        )
        .unwrap();
    let other_control = authority.grant_control(&other_view.id, NOW, 5_000).unwrap();
    assert_eq!(
        authority.authorize_control(&view_only_device, &session_id(), &media_id(), NOW),
        Err(GrantError::ControlGrantMissing)
    );
    assert_eq!(
        authority.authorize_control(
            &view_only_device,
            &session_id(),
            &media_id(),
            other_control.expires_at_ms
        ),
        Err(GrantError::ControlGrantMissing)
    );
}

#[test]
fn input_requires_view_control_and_a_live_lease() {
    let mut host = connected_host();
    let device_a = device("a");
    let view = host
        .grant_view(device_a.clone(), session_id(), NOW, 30_000)
        .unwrap();
    let control = host.grant_control(&view.id, NOW, 5_000).unwrap();
    let lease = host.claim_controller(&control.id, NOW, 2_000).unwrap();

    host.authorize_input(&device_a, &session_id(), &lease.id, NOW)
        .unwrap();

    // View-only device: no control grant.
    let view_only = device("view-only");
    host.grant_view(view_only.clone(), session_id(), NOW, 30_000)
        .unwrap();
    assert_eq!(
        host.authorize_input(&view_only, &session_id(), &lease.id, NOW),
        Err(InputDenial::MissingControlGrant)
    );

    // A different Session has no view grant for this media session.
    assert_eq!(
        host.authorize_input(&device_a, &other_session_id(), &lease.id, NOW),
        Err(InputDenial::MissingViewGrant)
    );

    // Expiry is absolute.
    assert_eq!(
        host.authorize_input(&device_a, &session_id(), &lease.id, NOW + 2_000),
        Err(InputDenial::ControllerLeaseExpired)
    );
}

#[test]
fn single_controller_binding_release_and_revoke_are_explicit() {
    let mut host = connected_host();
    let device_a = device("a");
    let device_b = device("b");
    let view_a = host
        .grant_view(device_a.clone(), session_id(), NOW, 30_000)
        .unwrap();
    let control_a = host.grant_control(&view_a.id, NOW, 5_000).unwrap();
    let lease_a = host.claim_controller(&control_a.id, NOW, 2_000).unwrap();

    let view_b = host
        .grant_view(device_b.clone(), session_id(), NOW, 30_000)
        .unwrap();
    let control_b = host.grant_control(&view_b.id, NOW, 5_000).unwrap();
    assert_eq!(
        host.claim_controller(&control_b.id, NOW, 2_000),
        Err(GrantError::ControllerAlreadyBound)
    );

    host.release_controller(&lease_a.id, &device_a).unwrap();
    assert_eq!(
        host.authorize_input(&device_a, &session_id(), &lease_a.id, NOW),
        Err(InputDenial::ControllerLeaseMissing)
    );
    let lease_b = host.claim_controller(&control_b.id, NOW, 2_000).unwrap();
    host.authorize_input(&device_b, &session_id(), &lease_b.id, NOW)
        .unwrap();

    // Revoking the view cascades to its control grant and lease.
    assert!(host.revoke_view(&view_b.id));
    assert_eq!(
        host.authorize_input(&device_b, &session_id(), &lease_b.id, NOW),
        Err(InputDenial::MissingViewGrant)
    );

    // Device revocation clears everything for that device.
    let removed = host.revoke_device(&device_a);
    assert!(removed >= 1);
    assert_eq!(
        host.authorize_view(&device_a, &session_id(), NOW),
        Err(GrantError::ViewGrantMissing)
    );
}

#[test]
fn lease_expiry_and_control_expiry_are_distinct_denials() {
    let mut host = connected_host();
    let device_a = device("a");

    let view = host
        .grant_view(device_a.clone(), session_id(), NOW, 30_000)
        .unwrap();
    let control = host.grant_control(&view.id, NOW, 10_000).unwrap();
    let lease = host.claim_controller(&control.id, NOW, 1_000).unwrap();
    assert_eq!(
        host.authorize_input(&device_a, &session_id(), &lease.id, NOW + 1_000),
        Err(InputDenial::ControllerLeaseExpired)
    );

    let mut host = connected_host();
    let view = host
        .grant_view(device_a.clone(), session_id(), NOW, 30_000)
        .unwrap();
    let control = host.grant_control(&view.id, NOW, 1_000).unwrap();
    let lease = host.claim_controller(&control.id, NOW, 1_000).unwrap();
    assert_eq!(
        host.authorize_input(&device_a, &session_id(), &lease.id, NOW + 1_000),
        Err(InputDenial::ControlGrantExpired)
    );
}

#[test]
fn pause_lock_and_reconnect_withdraw_the_lease_without_replay() {
    let mut host =
        ukk_media::HostSession::new(media_id(), NOW + SESSION_TTL, offer_capabilities(), sdp())
            .unwrap();
    let mut viewer = MediaSession::viewer(media_id(), viewer_capabilities(), sdp()).unwrap();
    let offer = host.start(NOW).unwrap();
    let answer = viewer.handle(&offer, NOW).unwrap().unwrap();
    host.handle(&answer, NOW).unwrap();

    let device_a = device("a");
    let view = host
        .grant_view(device_a.clone(), session_id(), NOW, 30_000)
        .unwrap();
    let control = host.grant_control(&view.id, NOW, 5_000).unwrap();
    let lease = host.claim_controller(&control.id, NOW, 2_000).unwrap();

    let pause = host.pause(NOW, PauseReason::UserPaused).unwrap();
    viewer.handle(&pause, NOW).unwrap();
    assert!(matches!(viewer.state(), MediaState::Paused(_)));
    assert_eq!(
        host.authorize_input(&device_a, &session_id(), &lease.id, NOW),
        Err(InputDenial::SessionPaused)
    );
    assert!(host.grants().controller_lease(&media_id()).is_none());

    let resume = host.resume(NOW).unwrap();
    viewer.handle(&resume, NOW).unwrap();
    assert_eq!(viewer.state(), &MediaState::Connected);
    assert_eq!(host.state(), &MediaState::Connected);
    // The old lease is gone; resuming never replays it.
    assert_eq!(
        host.authorize_input(&device_a, &session_id(), &lease.id, NOW),
        Err(InputDenial::ControllerLeaseMissing)
    );

    // Screen lock forces a pause and blocks resume until explicit unlock.
    let lock = host.lock(NOW).unwrap();
    viewer.handle(&lock, NOW).unwrap();
    assert_eq!(
        host.authorize_input(&device_a, &session_id(), &lease.id, NOW),
        Err(InputDenial::Locked)
    );
    assert!(viewer.state() == &MediaState::Paused(PauseReason::ScreenLocked));
    assert_eq!(host.resume(NOW), Err(SignallingError::Locked));
    host.unlock().unwrap();
    let resume = host.resume(NOW).unwrap();
    viewer.handle(&resume, NOW).unwrap();
    assert_eq!(host.state(), &MediaState::Connected);
    assert_eq!(viewer.state(), &MediaState::Connected);

    // Reconnect suspends media, withdraws the lease, then resumes cleanly.
    let lease = host.claim_controller(&control.id, NOW, 2_000).unwrap();
    let reconnect = host.suspend_for_reconnect(NOW).unwrap();
    viewer.handle(&reconnect, NOW).unwrap();
    assert_eq!(
        host.authorize_input(&device_a, &session_id(), &lease.id, NOW),
        Err(InputDenial::SessionPaused)
    );
    assert!(host.grants().controller_lease(&media_id()).is_none());
    let resumed = host.resume_after_reconnect(NOW).unwrap();
    viewer.handle(&resumed, NOW).unwrap();
    assert_eq!(host.state(), &MediaState::Connected);
    assert_eq!(
        host.authorize_input(&device_a, &session_id(), &lease.id, NOW),
        Err(InputDenial::ControllerLeaseMissing)
    );
}

#[test]
fn expired_view_cannot_mint_control_or_lease_and_lock_blocks_claims() {
    let mut authority = GrantAuthority::new();
    let device_a = device("a");
    let view = authority
        .grant_view(device_a.clone(), session_id(), media_id(), NOW, 1_000)
        .unwrap();
    assert_eq!(
        authority.grant_control(&view.id, NOW + 1_000, 1_000),
        Err(GrantError::ViewGrantExpired)
    );

    let view = authority
        .grant_view(
            device_a.clone(),
            session_id(),
            media_id(),
            NOW + 1_000,
            1_000,
        )
        .unwrap();
    let control = authority
        .grant_control(&view.id, NOW + 1_000, 1_000)
        .unwrap();
    assert_eq!(
        authority.claim_controller(&control.id, NOW + 2_000, 1_000),
        Err(GrantError::ControlGrantExpired)
    );

    let view = authority
        .grant_view(
            device_a.clone(),
            session_id(),
            media_id(),
            NOW + 2_000,
            30_000,
        )
        .unwrap();
    let control = authority
        .grant_control(&view.id, NOW + 2_000, 5_000)
        .unwrap();
    authority.lock_media(&media_id());
    assert_eq!(
        authority.authorize_control(&device_a, &session_id(), &media_id(), NOW + 2_000),
        Err(GrantError::Locked)
    );
    assert_eq!(
        authority.claim_controller(&control.id, NOW + 2_000, 1_000),
        Err(GrantError::Locked)
    );
    authority.unlock_media(&media_id());
    let lease = authority
        .claim_controller(&control.id, NOW + 2_000, 1_000)
        .unwrap();
    let renewed = authority
        .renew_controller(&lease.id, &device_a, NOW + 2_500, 1_000)
        .unwrap();
    assert_eq!(renewed.id, lease.id);
    assert!(renewed.expires_at_ms > lease.expires_at_ms);
    authority
        .authorize_control_with_lease(
            &device_a,
            &session_id(),
            &media_id(),
            &renewed.id,
            NOW + 2_500,
        )
        .unwrap();
}

#[test]
fn stop_is_terminal_and_denies_revived_grants() {
    let mut host = connected_host();
    let device_a = device("a");
    let view = host
        .grant_view(device_a.clone(), session_id(), NOW, 30_000)
        .unwrap();
    let control = host.grant_control(&view.id, NOW, 5_000).unwrap();
    let lease = host.claim_controller(&control.id, NOW, 2_000).unwrap();

    let stop = host.stop(StopReason::UserClosed).unwrap();
    assert_eq!(stop.kind(), "stop");
    assert!(matches!(
        host.state(),
        MediaState::Stopped(StopReason::UserClosed)
    ));
    assert_eq!(
        host.authorize_input(&device_a, &session_id(), &lease.id, NOW),
        Err(InputDenial::SessionStopped)
    );
    assert_eq!(
        host.grant_view(device_a.clone(), session_id(), NOW, 30_000),
        Err(GrantError::SessionClosed)
    );
    assert_eq!(
        host.stop(StopReason::UserClosed),
        Err(SignallingError::Terminal)
    );
}

#[test]
fn expiry_closes_the_session_and_revokes_input() {
    let mut host =
        ukk_media::HostSession::new(media_id(), NOW + 500, offer_capabilities(), sdp()).unwrap();
    let mut viewer = MediaSession::viewer(media_id(), viewer_capabilities(), sdp()).unwrap();
    let offer = host.start(NOW).unwrap();
    let answer = viewer.handle(&offer, NOW).unwrap().unwrap();
    host.handle(&answer, NOW).unwrap();

    let device_a = device("a");
    let view = host
        .grant_view(device_a.clone(), session_id(), NOW, 30_000)
        .unwrap();
    let control = host.grant_control(&view.id, NOW, 5_000).unwrap();
    let lease = host.claim_controller(&control.id, NOW, 2_000).unwrap();
    assert_eq!(
        host.authorize_input(&device_a, &session_id(), &lease.id, NOW + 500),
        Err(InputDenial::SessionExpired)
    );
    let candidate = host
        .add_ice_candidate(NOW + 500, candidate())
        .expect_err("expired");
    assert_eq!(candidate, SignallingError::Expired);
    assert!(matches!(
        host.state(),
        MediaState::Failed(FailureReason::Expired)
    ));
    assert!(host.grants().controller_lease(&media_id()).is_none());
}

#[test]
fn oversized_message_is_rejected_before_state_changes() {
    let host = connected_host();
    let stop = SignallingMessage::Stop {
        v: 1,
        media_session_id: media_id(),
        reason: StopReason::UserClosed,
    };
    let mut bytes = stop.encode().unwrap();
    bytes.extend(std::iter::repeat_n(b' ', MAX_SIGNALLING_MESSAGE_BYTES));
    assert!(matches!(
        SignallingMessage::decode(&bytes),
        Err(SignallingError::Oversized { .. })
    ));
    assert_eq!(host.state(), &MediaState::Connected);
}
