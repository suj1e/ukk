package hc

import (
	"context"
	"encoding/binary"
	"encoding/json"
	"sync"
	"time"
)

const (
	maxStreamBuffer = 4 << 20
	maxFactsBytes   = 64 << 10
	closeAckWait    = 2 * time.Second
	streamSendWait  = 5 * time.Second
)

// AttachOptions controls an attach stream. Observe never types, never resizes
// and never claims focus. AutomaticInput asks the caller to use
// Attachment.AutomaticWrite for terminal-generated replies; the capability is
// still confirmed by both hello and the attach reply before any kind-6 frame
// is sent. ResizeNotifications opts into core-to-observer RESIZE frames.
type AttachOptions struct {
	Observe             bool
	History             uint
	AutomaticInput      bool
	ResizeNotifications bool
	Caller              string
	Display             *Display
	Resumed             bool
}

type attachParams struct {
	Terminal            string   `json:"terminal"`
	Cols                uint16   `json:"cols"`
	Rows                uint16   `json:"rows"`
	Caller              string   `json:"caller,omitempty"`
	Display             *Display `json:"display,omitempty"`
	History             uint     `json:"history,omitempty"`
	Observe             bool     `json:"observe,omitempty"`
	ResizeNotifications bool     `json:"resize_notifications,omitempty"`
	Resumed             bool     `json:"resumed,omitempty"`
}

// Attachment is one framed terminal stream. Reads return raw terminal bytes.
type Attachment struct {
	client              *Client
	channel             uint32
	terminal            Terminal
	observe             bool
	automatic           bool
	resizeNotifications bool

	mu         sync.Mutex
	buf        []byte
	notify     chan struct{}
	done       chan struct{}
	closed     bool
	userClosed bool
	err        error
	closeInfo  *CloseInfo
	cols       uint16
	rows       uint16
	facts      []byte
	credited   uint64
	returned   uint64
}

func newAttachment(client *Client, result AttachResult, opts AttachOptions, cols, rows uint16) *Attachment {
	return &Attachment{
		client:              client,
		channel:             result.Channel,
		terminal:            result.Terminal,
		observe:             opts.Observe,
		automatic:           result.AutomaticInput && client.hello.HasFeature("automatic_input"),
		resizeNotifications: result.ResizeNotifications,
		notify:              make(chan struct{}),
		done:                make(chan struct{}),
		cols:                cols,
		rows:                rows,
	}
}

// Attach opens a raw stream to a live terminal. cols and rows are the viewer's
// size in cells.
func (c *Client) Attach(ctx context.Context, terminal string, cols, rows uint16, opts AttachOptions) (*Attachment, error) {
	params := attachParams{
		Terminal:            terminal,
		Cols:                cols,
		Rows:                rows,
		Caller:              opts.Caller,
		Display:             opts.Display,
		History:             opts.History,
		Observe:             opts.Observe,
		ResizeNotifications: opts.ResizeNotifications,
		Resumed:             opts.Resumed,
	}
	var att *Attachment
	prepare := func(raw json.RawMessage) (func(), error) {
		var result AttachResult
		if err := json.Unmarshal(raw, &result); err != nil {
			return nil, &Error{Code: CodeProtocol, Method: "terminal.attach", Err: err}
		}
		if result.Channel == 0 {
			return nil, &Error{Code: CodeProtocol, Method: "terminal.attach", Message: "attach reply has no channel"}
		}
		automatic := result.AutomaticInput && c.hello.HasFeature("automatic_input")
		if opts.AutomaticInput && !automatic {
			return nil, &Error{Code: CodeAutomaticUnavail, Method: "terminal.attach"}
		}
		a := newAttachment(c, result, opts, cols, rows)
		if err := c.registerAttachment(a); err != nil {
			return nil, err
		}
		att = a
		return func() { a.shutdown(&Error{Code: CodeAttachmentClosed}, nil) }, nil
	}
	if err := c.call(ctx, "terminal.attach", params, nil, prepare); err != nil {
		return nil, err
	}
	return att, nil
}

func (c *Client) registerAttachment(a *Attachment) error {
	select {
	case <-c.done:
		return c.connectionError()
	default:
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	select {
	case <-c.done:
		return c.connectionError()
	default:
	}
	if _, exists := c.attachments[a.channel]; exists {
		return &Error{Code: CodeProtocol, Method: "terminal.attach", Message: "duplicate stream channel"}
	}
	c.attachments[a.channel] = a
	return nil
}

// Channel is the framed stream channel.
func (a *Attachment) Channel() uint32 { return a.channel }

// Terminal is the terminal record from the attach reply.
func (a *Attachment) Terminal() Terminal { return a.terminal }

// Observe reports whether this stream is watch-only.
func (a *Attachment) Observe() bool { return a.observe }

// AutomaticInput reports whether kind-6 frames are negotiated on the whole
// path (hello and the attach reply).
func (a *Attachment) AutomaticInput() bool { return a.automatic }

// ResizeNotifications reports whether the core confirmed core-to-observer
// RESIZE before replacement paints.
func (a *Attachment) ResizeNotifications() bool { return a.resizeNotifications }

// Size is the last size known for the stream.
func (a *Attachment) Size() (cols, rows uint16) {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.cols, a.rows
}

// Done is closed when the stream ends locally or by a core CLOSE.
func (a *Attachment) Done() <-chan struct{} { return a.done }

// CloseInfo returns the core's typed close reason, if one arrived.
func (a *Attachment) CloseInfo() (CloseInfo, bool) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.closeInfo == nil {
		return CloseInfo{}, false
	}
	return *a.closeInfo, true
}

// Err is the terminal stream error, if any.
func (a *Attachment) Err() error {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.err
}

// CreditedBytes is how many stream bytes this attachment acknowledged with
// CREDIT frames.
func (a *Attachment) CreditedBytes() uint64 {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.credited
}

// Facts returns the last FACTS payload, if any.
func (a *Attachment) Facts() json.RawMessage {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.facts == nil {
		return nil
	}
	return append(json.RawMessage(nil), a.facts...)
}

// Read returns terminal bytes, blocking until data, stream close, or
// Attachment.Close.
func (a *Attachment) Read(p []byte) (int, error) {
	return a.ReadContext(context.Background(), p)
}

// ReadContext is Read with a context. Every byte returned is acknowledged
// with a CREDIT frame for exactly that many bytes.
func (a *Attachment) ReadContext(ctx context.Context, p []byte) (int, error) {
	if len(p) == 0 {
		return 0, nil
	}
	if ctx == nil {
		ctx = context.Background()
	}
	for {
		a.mu.Lock()
		if len(a.buf) > 0 {
			n := copy(p, a.buf)
			a.buf = a.buf[n:]
			if len(a.buf) == 0 {
				a.buf = nil
			}
			a.mu.Unlock()
			a.acknowledge(n)
			return n, nil
		}
		if a.err != nil {
			err := a.err
			a.mu.Unlock()
			return 0, err
		}
		wait := a.notify
		a.mu.Unlock()

		select {
		case <-wait:
		case <-a.done:
		case <-ctx.Done():
			return 0, ctx.Err()
		}
	}
}

func (a *Attachment) acknowledge(n int) {
	var payload [4]byte
	binary.LittleEndian.PutUint32(payload[:], uint32(n))
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	err := a.client.sendFrame(ctx, a.channel, KindCredit, payload[:])
	cancel()
	if err == nil {
		a.mu.Lock()
		a.credited += uint64(n)
		a.mu.Unlock()
	}
}

func (a *Attachment) push(payload []byte) {
	if len(payload) == 0 {
		return
	}
	a.mu.Lock()
	if a.closed {
		a.mu.Unlock()
		return
	}
	if len(a.buf)+len(payload) > maxStreamBuffer {
		a.mu.Unlock()
		a.shutdown(&Error{Code: CodeFlowControl, Method: "terminal.attach"}, nil)
		return
	}
	a.buf = append(a.buf, payload...)
	a.wakeLocked()
	a.mu.Unlock()
}

func (a *Attachment) wakeLocked() {
	close(a.notify)
	a.notify = make(chan struct{})
}

func (a *Attachment) shutdown(err error, info *CloseInfo) {
	a.mu.Lock()
	if a.closed {
		a.mu.Unlock()
		return
	}
	a.closed = true
	a.err = err
	a.closeInfo = info
	close(a.done)
	a.wakeLocked()
	a.mu.Unlock()
	a.client.removeAttachment(a.channel)
}

func (a *Attachment) handleCoreClose(payload []byte) {
	info := &CloseInfo{}
	if len(payload) > 0 {
		var parsed CloseInfo
		if err := json.Unmarshal(payload, &parsed); err == nil {
			info = &parsed
		}
	}
	a.shutdown(&StreamClosedError{Channel: a.channel, Close: *info}, info)
}

func (a *Attachment) handleCoreResize(payload []byte) {
	if len(payload) < 4 {
		return
	}
	a.mu.Lock()
	a.cols = binary.LittleEndian.Uint16(payload[0:2])
	a.rows = binary.LittleEndian.Uint16(payload[2:4])
	a.wakeLocked()
	a.mu.Unlock()
}

func (a *Attachment) handleFacts(payload []byte) {
	if len(payload) > maxFactsBytes {
		return
	}
	copied := append([]byte(nil), payload...)
	a.mu.Lock()
	a.facts = copied
	a.mu.Unlock()
}

func (a *Attachment) credit(n uint64) {
	a.mu.Lock()
	a.returned += n
	a.mu.Unlock()
}

// Write sends human input as DATA. Observe streams refuse it locally.
func (a *Attachment) Write(p []byte) (int, error) {
	if a.observe {
		return 0, &Error{Code: CodeObserveReadOnly, Method: "terminal.attach", Message: "observe stream cannot type"}
	}
	if len(p) == 0 {
		return 0, nil
	}
	if err := a.send(KindData, p); err != nil {
		return 0, err
	}
	return len(p), nil
}

// AutomaticWrite sends terminal-generated reply bytes as AUTOMATIC_INPUT. It
// refuses observe streams and paths that did not negotiate the capability.
func (a *Attachment) AutomaticWrite(p []byte) (int, error) {
	if a.observe {
		return 0, &Error{Code: CodeObserveReadOnly, Method: "terminal.attach", Message: "observe stream cannot type"}
	}
	if !a.automatic {
		return 0, &Error{Code: CodeAutomaticUnavail, Method: "terminal.attach"}
	}
	if len(p) == 0 {
		return 0, nil
	}
	if err := a.send(KindAutomaticInput, p); err != nil {
		return 0, err
	}
	return len(p), nil
}

// Resize sends a RESIZE frame. Observe streams refuse it locally.
func (a *Attachment) Resize(cols, rows uint16) error {
	if a.observe {
		return &Error{Code: CodeObserveReadOnly, Method: "terminal.attach", Message: "observe stream cannot resize"}
	}
	var payload [4]byte
	binary.LittleEndian.PutUint16(payload[0:2], cols)
	binary.LittleEndian.PutUint16(payload[2:4], rows)
	if err := a.send(KindResize, payload[:]); err != nil {
		return err
	}
	a.mu.Lock()
	a.cols, a.rows = cols, rows
	a.mu.Unlock()
	return nil
}

// Focus reports focus gain or loss. Observe streams refuse it locally.
func (a *Attachment) Focus(focused bool) error {
	if a.observe {
		return &Error{Code: CodeObserveReadOnly, Method: "terminal.attach", Message: "observe stream cannot take focus"}
	}
	payload := []byte{0}
	if focused {
		payload[0] = 1
	}
	return a.send(KindFocus, payload)
}

// SendFacts sends a FACTS frame with the viewer's display facts.
func (a *Attachment) SendFacts(display Display) error {
	body, err := json.Marshal(display)
	if err != nil {
		return &Error{Code: CodeProtocol, Method: "terminal.attach", Err: err}
	}
	if len(body) > maxRequestJSON {
		return &Error{Code: CodeRequestTooLarge, Method: "terminal.attach"}
	}
	if err := a.send(KindFacts, body); err != nil {
		return err
	}
	return nil
}

func (a *Attachment) send(kind uint8, payload []byte) error {
	ctx, cancel := context.WithTimeout(context.Background(), streamSendWait)
	defer cancel()
	return a.client.sendFrame(ctx, a.channel, kind, payload)
}

// Close detaches this view by sending CLOSE and waits briefly for the core's
// CLOSE acknowledgement. It never stops the terminal.
func (a *Attachment) Close() error {
	a.mu.Lock()
	if a.closed {
		a.mu.Unlock()
		return nil
	}
	if a.userClosed {
		done := a.done
		a.mu.Unlock()
		select {
		case <-done:
			return nil
		case <-time.After(closeAckWait):
			return &Error{Code: CodeCloseTimeout, Method: "terminal.attach"}
		}
	}
	a.userClosed = true
	a.mu.Unlock()

	ctx, cancel := context.WithTimeout(context.Background(), closeAckWait)
	err := a.client.sendFrame(ctx, a.channel, KindClose, nil)
	cancel()
	if err != nil {
		a.shutdown(&Error{Code: CodeConnection, Method: "terminal.attach", Err: err}, nil)
		return nil
	}
	select {
	case <-a.done:
		return nil
	case <-time.After(closeAckWait):
		a.shutdown(&Error{Code: CodeCloseTimeout, Method: "terminal.attach"}, nil)
		return &Error{Code: CodeCloseTimeout, Method: "terminal.attach"}
	}
}
