package hc

import (
	"encoding/json"
	"errors"
	"fmt"
	"time"
)

// ErrorCode is a stable, branchable error code. Callers must not match on
// human-readable text.
type ErrorCode string

const (
	CodeClosed             ErrorCode = "closed"
	CodeCanceled           ErrorCode = "canceled"
	CodeTimeout            ErrorCode = "timeout"
	CodeConnection         ErrorCode = "connection_lost"
	CodeSpawn              ErrorCode = "spawn_failed"
	CodeHandshake          ErrorCode = "handshake_failed"
	CodeUnsupported        ErrorCode = "unsupported_generation"
	CodeProtocol           ErrorCode = "protocol_error"
	CodeRPC                ErrorCode = "rpc_error"
	CodePayloadTooLarge    ErrorCode = "payload_too_large"
	CodeRequestTooLarge    ErrorCode = "request_too_large"
	CodeTooManyPending     ErrorCode = "too_many_pending"
	CodeBackpressure       ErrorCode = "backpressure"
	CodeObserveReadOnly    ErrorCode = "observe_read_only"
	CodeAutomaticUnavail   ErrorCode = "automatic_input_unavailable"
	CodeAttachmentClosed   ErrorCode = "attachment_closed"
	CodeCloseTimeout       ErrorCode = "close_timeout"
	CodeFlowControl        ErrorCode = "flow_control"
	CodeSubscriptionClosed ErrorCode = "subscription_closed"
	CodeEventOverflow      ErrorCode = "event_queue_overflow"
)

// ErrProtocol is wrapped by malformed wire data. Use IsCode for branching.
var ErrProtocol = &Error{Code: CodeProtocol}

// Error is the package's typed error. The server's free-form message is kept
// in Message for local inspection but is deliberately not part of Error():
// messages can quote terminal contents, file paths or other data that must not
// appear in logs.
type Error struct {
	Code    ErrorCode
	Method  string
	Kind    string // stable herdr-core error kind, e.g. not_found
	RPCCode int
	Message string
	Err     error
}

func (e *Error) Error() string {
	if e == nil {
		return "hc: <nil>"
	}
	switch {
	case e.Kind != "" && e.Method != "":
		return fmt.Sprintf("hc: %s: %s (%s)", e.Code, e.Kind, e.Method)
	case e.Kind != "":
		return fmt.Sprintf("hc: %s: %s", e.Code, e.Kind)
	case e.Method != "":
		return fmt.Sprintf("hc: %s (%s)", e.Code, e.Method)
	default:
		return "hc: " + string(e.Code)
	}
}

func (e *Error) Unwrap() error { return e.Err }

// IsCode reports whether err (or an error it wraps) carries code.
func IsCode(err error, code ErrorCode) bool {
	var target *Error
	return errors.As(err, &target) && target.Code == code
}

// GapError reports an event sequence gap on an unfiltered subscription.
// Recovered is true when a `missed` snapshot reset the sequence.
type GapError struct {
	Expected  uint64
	Got       uint64
	Recovered bool
}

func (e *GapError) Error() string {
	if e.Recovered {
		return fmt.Sprintf("hc: events missed; snapshot at %d", e.Got)
	}
	return fmt.Sprintf("hc: event sequence gap: expected %d, got %d", e.Expected, e.Got)
}

// Options configures a connection to `hc connect`.
type Options struct {
	// Binary is the hc executable. Empty means "hc" from PATH.
	Binary string
	// Args are extra arguments appended after "connect".
	Args []string
	// Timeout bounds the hello handshake and, when non-zero, is also the
	// default deadline for a call whose context has none.
	Timeout time.Duration
	// Env holds extra environment variables in KEY=VALUE form, appended to
	// the current process environment.
	Env []string
}

// HelloResult is the hello reply (protocol generation 1).
type HelloResult struct {
	Generation uint32   `json:"generation"`
	Version    string   `json:"version"`
	Core       CoreInfo `json:"core"`
	Framed     bool     `json:"framed"`
	Methods    []string `json:"methods"`
	Features   []string `json:"features"`
}

// CoreInfo identifies the herdr-core process.
type CoreInfo struct {
	ID   string `json:"id"`
	Name string `json:"name"`
}

// HasFeature reports whether the core advertised a hello feature.
func (h HelloResult) HasFeature(name string) bool {
	for _, f := range h.Features {
		if f == name {
			return true
		}
	}
	return false
}

// Display is the viewer's colors and cell size.
type Display struct {
	FG     *string  `json:"fg"`
	BG     *string  `json:"bg"`
	Cursor *string  `json:"cursor"`
	Cell   []uint32 `json:"cell"`
}

// InitialSize is a terminal's initial cell grid.
type InitialSize struct {
	Cols uint16 `json:"cols"`
	Rows uint16 `json:"rows"`
}

// Status is one OSC 7501 record as reported by the program.
type Status struct {
	State    string  `json:"state"`
	App      string  `json:"app,omitempty"`
	Title    string  `json:"title,omitempty"`
	Msg      string  `json:"msg,omitempty"`
	Kind     *string `json:"kind,omitempty"`
	Progress *uint8  `json:"progress,omitempty"`
}

// Foreground is the current foreground process group leader.
type Foreground struct {
	Pid  int32    `json:"pid"`
	Name string   `json:"name"`
	Argv []string `json:"argv"`
	Cwd  string   `json:"cwd"`
}

// FirstMessage records what happened to a template's first message.
type FirstMessage struct {
	Sent    bool    `json:"sent"`
	Reason  *string `json:"reason,omitempty"`
	Started *bool   `json:"started,omitempty"`
	Turn    *uint64 `json:"turn,omitempty"`
}

// WaitResult is how a wait ended.
type WaitResult struct {
	Terminal string       `json:"terminal"`
	Ended    bool         `json:"ended"`
	Reason   string       `json:"reason"`
	Status   *string      `json:"status,omitempty"`
	ExitCode *int32       `json:"exit_code,omitempty"`
	Signal   *int32       `json:"signal,omitempty"`
	Turn     uint64       `json:"turn"`
	From     *string      `json:"from,omitempty"`
	Children []WaitResult `json:"children"`
}

// Terminal is one terminal's facts, live or finished.
type Terminal struct {
	Address       string            `json:"address"`
	Labels        map[string]string `json:"labels"`
	Argv          []string          `json:"argv"`
	Cwd           string            `json:"cwd"`
	State         string            `json:"state"`
	CreatedMS     uint64            `json:"created_ms"`
	Screen        string            `json:"screen"`
	Cols          uint16            `json:"cols"`
	Rows          uint16            `json:"rows"`
	Name          *string           `json:"name,omitempty"`
	DieWithParent bool              `json:"die_with_parent,omitempty"`
	ExitCode      *int32            `json:"exit_code,omitempty"`
	FinishedMS    *uint64           `json:"finished_ms,omitempty"`
	Identity      string            `json:"identity,omitempty"`
	Keep          bool              `json:"keep,omitempty"`
	Notify        bool              `json:"notify,omitempty"`
	Parent        *string           `json:"parent,omitempty"`
	PasteGapMS    *uint64           `json:"paste_gap_ms,omitempty"`
	Pid           *uint32           `json:"pid,omitempty"`
	Reason        *string           `json:"reason,omitempty"`
	Reported      bool              `json:"reported,omitempty"`
	Rm            bool              `json:"rm,omitempty"`
	Signal        *int32            `json:"signal,omitempty"`
	Status        *Status           `json:"status,omitempty"`
	StatusMS      *uint64           `json:"status_ms,omitempty"`
	Stop          *string           `json:"stop,omitempty"`
	Submit        *string           `json:"submit,omitempty"`
	Template      *string           `json:"template,omitempty"`
	Title         string            `json:"title,omitempty"`
	Turn          uint64            `json:"turn,omitempty"`
	Foreground    *Foreground       `json:"foreground,omitempty"`
	Records       map[string]Status `json:"records,omitempty"`
	FirstMessage  *FirstMessage     `json:"first_message,omitempty"`
}

// RunResult is terminal.run's answer. It embeds the terminal record and adds
// the wait result and collected output.
type RunResult struct {
	Terminal
	Output *string     `json:"output,omitempty"`
	Wait   *WaitResult `json:"wait,omitempty"`
}

// ListResult is terminal.list's answer.
type ListResult struct {
	Terminals      []Terminal    `json:"terminals"`
	HiddenFinished uint64        `json:"hidden_finished"`
	Machines       []MachineView `json:"machines"`
}

// MachineView is a machine as ls shows it.
type MachineView struct {
	Name      string  `json:"name"`
	Target    string  `json:"target"`
	ID        string  `json:"id"`
	Host      string  `json:"host"`
	Version   string  `json:"version"`
	State     string  `json:"state"`
	Session   *string `json:"session,omitempty"`
	SinceMS   *uint64 `json:"since_ms,omitempty"`
	Why       *string `json:"why,omitempty"`
	Temporary bool    `json:"temporary,omitempty"`
	Via       bool    `json:"via,omitempty"`
}

// HolderInfo is one core holder.
type HolderInfo struct {
	Pid       uint32 `json:"pid"`
	Instance  string `json:"instance"`
	Current   bool   `json:"current"`
	Terminals uint64 `json:"terminals"`
}

// StatusResult is server.status's answer.
type StatusResult struct {
	Core        CoreInfo     `json:"core"`
	Version     string       `json:"version"`
	Pid         uint32       `json:"pid"`
	UptimeMS    uint64       `json:"uptime_ms"`
	Holder      *HolderInfo  `json:"holder,omitempty"`
	Holders     []HolderInfo `json:"holders"`
	InstalledBy *string      `json:"installed_by"`
	Live        uint64       `json:"live"`
	Finished    uint64       `json:"finished"`
}

// ReadResult is terminal.read's answer.
type ReadResult struct {
	Text     string  `json:"text"`
	Complete bool    `json:"complete"`
	How      string  `json:"how"`
	Why      *string `json:"why,omitempty"`
}

// SendResult is terminal.send's answer.
type SendResult struct {
	Turn    uint64      `json:"turn"`
	Status  *string     `json:"status,omitempty"`
	Started *bool       `json:"started,omitempty"`
	Wait    *WaitResult `json:"wait,omitempty"`
}

// KeysResult is terminal.keys's answer.
type KeysResult struct {
	Turn   uint64  `json:"turn"`
	Status *string `json:"status,omitempty"`
}

// InterruptResult is terminal.interrupt's answer.
type InterruptResult struct {
	Outcome string `json:"outcome"`
	Key     string `json:"key"`
}

// RemoveResult is terminal.rm's answer.
type RemoveResult struct {
	Removed string `json:"removed"`
}

// AttachResult is terminal.attach's answer.
type AttachResult struct {
	Channel             uint32   `json:"channel"`
	Terminal            Terminal `json:"terminal"`
	ResizeNotifications bool     `json:"resize_notifications"`
	AutomaticInput      bool     `json:"automatic_input"`
}

// SubscribeResult is events.subscribe's answer.
type SubscribeResult struct {
	Channel   *uint32    `json:"channel"`
	Seq       uint64     `json:"seq"`
	Terminals []Terminal `json:"terminals"`
}

// CloseInfo is the JSON reason the core attaches to a CLOSE frame. Pointers
// distinguish null from zero.
type CloseInfo struct {
	Reason     string  `json:"reason"`
	ExitCode   *int32  `json:"exit_code"`
	Signal     *int32  `json:"signal"`
	FinishedMS *uint64 `json:"finished_ms"`
}

// StreamClosedError is returned by Attachment.Read once the core has closed
// the stream. Inspect Close for the typed reason.
type StreamClosedError struct {
	Channel uint32
	Close   CloseInfo
}

func (e *StreamClosedError) Error() string {
	if e == nil {
		return "hc: stream closed"
	}
	if e.Close.Reason == "" {
		return "hc: stream closed"
	}
	return "hc: stream closed: " + e.Close.Reason
}

// TerminalListOptions are terminal.list parameters.
type TerminalListOptions struct {
	All    bool              `json:"all,omitempty"`
	Labels map[string]string `json:"labels,omitempty"`
}

// RunOptions are terminal.run parameters.
type RunOptions struct {
	Argv          []string          `json:"argv,omitempty"`
	Caller        string            `json:"caller,omitempty"`
	Cwd           string            `json:"cwd,omitempty"`
	DieWithParent bool              `json:"die_with_parent,omitempty"`
	Display       *Display          `json:"display,omitempty"`
	Env           map[string]string `json:"env,omitempty"`
	Keep          bool              `json:"keep,omitempty"`
	Labels        map[string]string `json:"labels,omitempty"`
	Line          string            `json:"line,omitempty"`
	Name          string            `json:"name,omitempty"`
	NoNotify      bool              `json:"no_notify,omitempty"`
	NoParent      bool              `json:"no_parent,omitempty"`
	On            string            `json:"on,omitempty"`
	OutliveParent bool              `json:"outlive_parent,omitempty"`
	Parent        string            `json:"parent,omitempty"`
	Rm            bool              `json:"rm,omitempty"`
	Size          *InitialSize      `json:"size,omitempty"`
	Timeout       *float64          `json:"timeout,omitempty"`
	Wait          bool              `json:"wait,omitempty"`
}

// SendOptions are terminal.send parameters.
type SendOptions struct {
	Terminal string   `json:"terminal"`
	Text     string   `json:"text"`
	Caller   string   `json:"caller,omitempty"`
	Force    bool     `json:"force,omitempty"`
	If       string   `json:"if,omitempty"`
	Timeout  *float64 `json:"timeout,omitempty"`
	Wait     bool     `json:"wait,omitempty"`
}

// InterruptOptions are terminal.interrupt parameters.
type InterruptOptions struct {
	Terminal string   `json:"terminal"`
	Timeout  *float64 `json:"timeout,omitempty"`
}

// SubscribeOptions are events.subscribe parameters. All empty means an
// unfiltered subscription (the only kind for which sequence gaps are checked).
type SubscribeOptions struct {
	Terminal   string            `json:"terminal,omitempty"`
	ChildrenOf string            `json:"children_of,omitempty"`
	Labels     map[string]string `json:"labels,omitempty"`
}

// Filtered reports whether the subscription excludes some events.
func (o SubscribeOptions) Filtered() bool {
	return o.Terminal != "" || o.ChildrenOf != "" || len(o.Labels) != 0
}

// Event is one fact change. Data is the raw `params.data` value so callers can
// apply the event-type-specific record without a lossy intermediate type.
type Event struct {
	Seq      uint64          `json:"seq"`
	Type     string          `json:"type"`
	Terminal string          `json:"terminal,omitempty"`
	Machine  string          `json:"machine,omitempty"`
	Data     json.RawMessage `json:"data"`
}
