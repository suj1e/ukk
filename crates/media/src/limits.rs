//! Explicit bounds for the Remote Screen signalling contract.
//!
//! These are enforced before parsing, before encoding and again on the state
//! machine boundary. A signalling envelope never carries a media frame; the
//! bounds here are for SDP, ICE candidates, ICE server URLs and credentials
//! only.

/// Version of the ukk media signalling envelope.
pub const SIGNALLING_VERSION: u8 = 1;

/// Hard bound on one encoded signalling message.
pub const MAX_SIGNALLING_MESSAGE_BYTES: usize = 64 * 1024;

/// Hard bound on one SDP body.
pub const MAX_SDP_BYTES: usize = 16 * 1024;

/// Hard bound on one ICE candidate line.
pub const MAX_ICE_CANDIDATE_BYTES: usize = 1024;

/// Hard bound on one STUN/TURN URL.
pub const MAX_ICE_URL_BYTES: usize = 2048;

/// Hard bound on a TURN username or credential.
pub const MAX_CREDENTIAL_BYTES: usize = 512;

/// Hard bound on a codec name.
pub const MAX_CODEC_NAME_BYTES: usize = 64;

/// Maximum codecs in one capability descriptor.
pub const MAX_CAPABILITY_CODECS: usize = 16;

/// Maximum ICE servers in one capability descriptor.
pub const MAX_ICE_SERVERS: usize = 8;

/// Maximum URLs in one ICE server entry.
pub const MAX_ICE_URLS_PER_SERVER: usize = 8;

/// Maximum accepted absolute lifetime of a media session.
pub const MAX_MEDIA_SESSION_MS: i64 = 60 * 60 * 1000;

/// Maximum lifetime of a view grant.
pub const MAX_VIEW_GRANT_MS: i64 = 60 * 60 * 1000;

/// Maximum lifetime of a control grant.
pub const MAX_CONTROL_GRANT_MS: i64 = 10 * 60 * 1000;

/// Maximum lifetime of a controller lease.
pub const MAX_CONTROLLER_LEASE_MS: i64 = 10 * 60 * 1000;

/// Maximum number of live view/control grants held by one authority.
pub const MAX_ACTIVE_GRANTS: usize = 64;
