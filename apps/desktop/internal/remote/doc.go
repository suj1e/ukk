// Package remote implements ukk device identity, short-lived pairing and the
// end-to-end encrypted session protocol shared by the Go Desktop/Bridge and the
// Swift/iOS companion.
//
// The wire format is deliberately small and fully specified here so a second
// implementation can reproduce it with CryptoKit.
//
// # Invite
//
// An invite is one JSON object (no trailing data, unknown fields rejected):
//
//	{"v":1,"host_id":"…","host_ed":"…","host_x":"…","invite_id":"…",
//	 "secret":"…","expire_ms":1762550000000,"relay":"wss://relay.example/ws"}
//
// All byte strings use unpadded base64url (RFC 4648 §5). host_ed and host_x are
// 32-byte Ed25519 and X25519 public keys, secret is 32 random bytes, and
// expire_ms is an absolute Unix time in milliseconds. Relay URLs must be
// absolute wss:// URLs without userinfo. An invite expires after at most five
// minutes, is single use, and is marked in progress before any handshake work.
//
// # Handshake
//
// C1 and S1/S2 are JSON objects carried as ordered messages:
//
//	C1 {"t":"c1","invite_id":…,"ed":…,"x":…,"eph":…,"nc":…,"sig":…}
//	S1 {"t":"s1","host_ed":…,"host_x":…,"eph":…,"ns":…,"sig":…,"proof":…}
//	C2 {"t":"c2","proof":…}
//
// nc and ns are 32 random bytes. C1.sig is Ed25519(client_ed) over the
// client-visible transcript prefix:
//
//	clientPrefix = SHA256("ukk-handshake-v1" || host_id || invite_id ||
//	    host_ed || host_x || client_ed || client_x || eph_client || expire)
//
// The full transcript, used for key derivation, proofs and S1.sig, is:
//
//	transcript = SHA256("ukk-handshake-v1" || host_id || invite_id ||
//	    host_ed || host_x || client_ed || client_x ||
//	    eph_host || eph_client || expire)
//
// Identifiers are their UTF-8 bytes, keys are 32 raw bytes and expire is
// 8-byte big-endian Unix milliseconds. The client cannot sign the full
// transcript in C1 because eph_host does not exist until S1; S1.sig covers the
// full transcript and the C2 AEAD proof is bound to it.
//
// Shared secrets (each side uses its own private key and the peer public key):
//
//	ikm = DH(eph_host, eph_client) || DH(host_x, eph_client) ||
//	      DH(eph_host, client_x) || invite_secret
//
// Then:
//
//	salt = SHA256("ukk-salt-v1" || transcript)
//	HKDF-SHA256(ikm, salt, info="ukk-confirm|ukk-c2s|ukk-s2c", 96)
//	  -> k_confirm (32) || k_c2s (32) || k_s2c (32)
//	HKDF-SHA256(ikm, salt, info="ukk-code", 4) -> u32be mod 1000000 -> %06d
//
// Proofs are ChaCha20-Poly1305 with the empty plaintext, aad = transcript and
//
//	nonce = SHA256(transcript || "ukk-s1-proof")[0:12]   (S1)
//	nonce = SHA256(transcript || "ukk-c2-proof")[0:12]   (C2)
//
// The six-digit code is shown on both sides. Pairing succeeds only after the
// host verifies the C2 proof; any failure burns the invite.
//
// # Resume
//
// A paired device reconnects with fresh ephemerals and nonces; channel keys
// are never persisted or reused.
//
//	R1 {"t":"r1","host_id":…,"device_ed":…,"device_x":…,"eph":…,"nc":…,"sig":…}
//	R2 {"t":"r2","host_ed":…,"host_x":…,"eph":…,"ns":…,"sig":…,"proof":…}
//	R3 {"t":"r3","proof":…}
//
// resumePrefix = SHA256("ukk-resume-v1" || host_id || device_ed || device_x ||
//
//	eph_client || nc)
//
// R1.sig = Ed25519(device_ed) over resumePrefix. The full transcript is:
//
//	transcript = SHA256("ukk-resume-v1" || host_id || device_ed || device_x ||
//	    eph_host || eph_client || nc || ns)
//
// R2.sig = Ed25519(host) over transcript. The device verifies that R2's
// host_ed/host_x equal its pinned values, then verifies the signature and
// proof before sending R3. The host looks up device_ed in its persisted trust
// registry, rejects unknown/revoked/expired records and stale key generations,
// and installs the session only after R3 verifies.
//
//	ikm = DH(eph_host,eph_client) || DH(host_x,eph_client) || DH(eph_host,device_x)
//	salt = SHA256("ukk-salt-v1" || transcript)
//	HKDF-SHA256(ikm, salt, info="ukk-confirm|ukk-c2s|ukk-s2c", 96)
//	proof R2 = AEAD(k_confirm, aad=transcript, nonce=SHA256(transcript||"ukk-r2-proof")[0:12])
//	proof R3 = AEAD(k_confirm, aad=transcript, nonce=SHA256(transcript||"ukk-r3-proof")[0:12])
//
// There is no invite secret and no human confirmation code on resume;
// Session.ExpireMS is zero. nc and ns are 32 raw bytes each.
//
// # Envelope
//
// A session envelope is:
//
//	version(1) || stream(4, big-endian) || counter(8, big-endian) ||
//	ChaCha20-Poly1305(k, nonce=counter||stream, aad=header, plaintext)
//
// The header is the first 13 bytes. c2s and s2c keys are separate. Counters
// start at 1 and a bounded replay window rejects duplicates, old counters and
// gaps beyond the window. Plaintext larger than 1 MiB is refused.
package remote
