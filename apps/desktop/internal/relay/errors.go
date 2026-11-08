package relay

import (
	"errors"
	"fmt"
)

// Code is a stable machine-readable relay failure reason.
type Code string

const (
	CodeConfigInvalid        Code = "config_invalid"
	CodeUnauthorized         Code = "unauthorized"
	CodeBadRequest           Code = "bad_request"
	CodeFrameInvalid         Code = "frame_invalid"
	CodeDuplicateHello       Code = "duplicate_hello"
	CodeRoleMismatch         Code = "role_mismatch"
	CodeHostAlreadyConnected Code = "host_already_connected"
	CodeHostOffline          Code = "host_offline"
	CodeUnknownDevice        Code = "unknown_device"
	CodeDeviceRevoked        Code = "device_revoked"
	CodeDeviceLimit          Code = "device_limit"
	CodeHostDeviceLimit      Code = "host_device_limit"
	CodeConnLimit            Code = "conn_limit"
	CodeUnknownConn          Code = "unknown_conn"
	CodeConnNotOpen          Code = "conn_not_open"
	CodeDuplicateConn        Code = "duplicate_conn"
	CodePayloadTooLarge      Code = "payload_too_large"
	CodeQueueOverflow        Code = "queue_overflow"
	CodeIdleTimeout          Code = "idle_timeout"
	CodeRevoked              Code = "revoked"
	CodeClosed               Code = "closed"
)

// Error is a typed relay protocol failure. Message never contains payload
// bytes or credentials.
type Error struct {
	Code    Code
	Message string
	fatal   bool
}

func (e *Error) Error() string {
	if e == nil {
		return "relay: <nil>"
	}
	if e.Message == "" {
		return "relay: " + string(e.Code)
	}
	return "relay: " + string(e.Code) + ": " + e.Message
}

// Fatal reports whether the peer connection must close after this error.
func (e *Error) Fatal() bool { return e != nil && e.fatal }

func newError(code Code, format string, args ...any) *Error {
	return &Error{Code: code, Message: fmt.Sprintf(format, args...)}
}

func newFatal(code Code, format string, args ...any) *Error {
	return &Error{Code: code, Message: fmt.Sprintf(format, args...), fatal: true}
}

// CodeOf returns the stable code carried by err, or "".
func CodeOf(err error) Code {
	var e *Error
	if errors.As(err, &e) {
		return e.Code
	}
	return ""
}

// HasCode reports whether err carries code.
func HasCode(err error, code Code) bool { return CodeOf(err) == code }
