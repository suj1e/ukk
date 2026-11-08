package remote

import (
	"errors"
	"fmt"
)

// Code is a stable machine-readable failure reason. Callers branch on codes,
// never on error text.
type Code string

const (
	CodeIdentityInvalid  Code = "identity_invalid"
	CodeInviteInvalid    Code = "invite_invalid"
	CodeInviteExpired    Code = "invite_expired"
	CodeInviteUsed       Code = "invite_used"
	CodeInviteInProgress Code = "invite_in_progress"
	CodeInviteUnknown    Code = "invite_unknown"

	CodeHandshakeMalformed Code = "handshake_malformed"
	CodeHandshakeIdentity  Code = "handshake_identity"
	CodeHandshakeSignature Code = "handshake_signature"
	CodeHandshakeProof     Code = "handshake_proof"
	CodeHandshakeKey       Code = "handshake_key"

	CodeEnvelopeInvalid Code = "envelope_invalid"
	CodeEnvelopeVersion Code = "envelope_version"
	CodeEnvelopeSize    Code = "envelope_size"
	CodeEnvelopeAuth    Code = "envelope_auth"
	CodeEnvelopeReplay  Code = "envelope_replay"
	CodeEnvelopeStream  Code = "envelope_stream"

	CodeTransport Code = "transport"
	CodeClosed    Code = "closed"

	CodeUnknownDevice     Code = "unknown_device"
	CodeDeviceExists      Code = "device_exists"
	CodeDeviceRevoked     Code = "device_revoked"
	CodeStaleIdentity     Code = "stale_identity"
	CodeMissingScope      Code = "missing_scope"
	CodeGrantExpired      Code = "grant_expired"
	CodeGrantInvalid      Code = "grant_invalid"
	CodeGrantNotGrantable Code = "grant_not_grantable"
	CodeScopeUnknown      Code = "scope_unknown"
	CodeSubjectInvalid    Code = "subject_invalid"
)

// Error is a typed remote-protocol failure. Message never contains key
// material, invite secrets or plaintext.
type Error struct {
	Code    Code
	Message string
	Err     error
}

func (e *Error) Error() string {
	if e == nil {
		return "remote: <nil>"
	}
	if e.Message == "" {
		return "remote: " + string(e.Code)
	}
	return "remote: " + string(e.Code) + ": " + e.Message
}

func (e *Error) Unwrap() error { return e.Err }

func newError(code Code, format string, args ...any) *Error {
	return &Error{Code: code, Message: fmt.Sprintf(format, args...)}
}

func wrapError(code Code, message string, err error) *Error {
	return &Error{Code: code, Message: message, Err: err}
}

// CodeOf returns the stable code carried by err, or "" when err is not a
// remote protocol error.
func CodeOf(err error) Code {
	var e *Error
	if errors.As(err, &e) {
		return e.Code
	}
	return ""
}

// HasCode reports whether err carries code.
func HasCode(err error, code Code) bool { return CodeOf(err) == code }
