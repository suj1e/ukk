package hc

import (
	"bytes"
	"context"
	"encoding/json"
	"sync"
	"time"
)

const (
	maxEventLineBuffer = 1 << 20
	eventQueueDepth    = 256
	eventErrorDepth    = 16
)

// Subscription is one events.subscribe stream. Events() delivers fact changes
// in order; Snapshot() reports the latest authoritative snapshot (initially
// the subscribe reply, replaced on `missed`).
type Subscription struct {
	client   *Client
	channel  *uint32
	filtered bool

	mu       sync.Mutex
	initial  []Terminal
	snapshot []Terminal
	lastSeq  uint64
	missed   uint64
	events   chan Event
	errors   chan error
	done     chan struct{}
	closed   bool

	lineMu     sync.Mutex
	lineBuf    []byte
	discarding bool
}

// SubscribeEvents subscribes to terminal fact changes. The returned
// subscription is registered before the reply reaches the caller, so no
// notification can race the snapshot.
func (c *Client) SubscribeEvents(ctx context.Context, opts SubscribeOptions) (*Subscription, error) {
	var sub *Subscription
	prepare := func(raw json.RawMessage) (func(), error) {
		var result SubscribeResult
		if err := json.Unmarshal(raw, &result); err != nil {
			return nil, &Error{Code: CodeProtocol, Method: "events.subscribe", Err: err}
		}
		if result.Channel != nil && *result.Channel == 0 {
			return nil, &Error{Code: CodeProtocol, Method: "events.subscribe", Message: "event reply has channel 0"}
		}
		s := &Subscription{
			client:   c,
			channel:  result.Channel,
			filtered: opts.Filtered(),
			initial:  append([]Terminal(nil), result.Terminals...),
			snapshot: append([]Terminal(nil), result.Terminals...),
			lastSeq:  result.Seq,
			events:   make(chan Event, eventQueueDepth),
			errors:   make(chan error, eventErrorDepth),
			done:     make(chan struct{}),
		}
		if err := c.registerSubscription(s); err != nil {
			close(s.done)
			close(s.events)
			close(s.errors)
			return nil, err
		}
		sub = s
		return func() { s.closeLocal(&Error{Code: CodeSubscriptionClosed, Method: "events.subscribe"}) }, nil
	}
	if err := c.call(ctx, "events.subscribe", opts, nil, prepare); err != nil {
		return nil, err
	}
	return sub, nil
}

func (c *Client) registerSubscription(s *Subscription) error {
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
	if s.channel == nil {
		if c.plainSub != nil {
			return &Error{Code: CodeProtocol, Method: "events.subscribe", Message: "plain subscription already open"}
		}
		c.plainSub = s
		return nil
	}
	if _, exists := c.subscriptions[*s.channel]; exists {
		return &Error{Code: CodeProtocol, Method: "events.subscribe", Message: "duplicate event channel"}
	}
	c.subscriptions[*s.channel] = s
	return nil
}

// Events is the notification stream. It is closed when the subscription ends.
func (s *Subscription) Events() <-chan Event { return s.events }

// Errors reports sequence gaps, missed snapshots and protocol errors. It is
// bounded and closed when the subscription ends.
func (s *Subscription) Errors() <-chan error { return s.errors }

// Done is closed when the subscription ends.
func (s *Subscription) Done() <-chan struct{} { return s.done }

// Snapshot returns the latest matching terminals.
func (s *Subscription) Snapshot() []Terminal {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([]Terminal(nil), s.snapshot...)
}

// InitialSnapshot is the subscribe reply's snapshot, unaffected by later
// `missed` replacements.
func (s *Subscription) InitialSnapshot() []Terminal {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([]Terminal(nil), s.initial...)
}

// Seq is the last sequence applied or the snapshot's sequence.
func (s *Subscription) Seq() uint64 {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.lastSeq
}

// MissedCount is how many `missed` snapshots replaced the view.
func (s *Subscription) MissedCount() uint64 {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.missed
}

// Channel is the framed notification channel, or nil in plain mode.
func (s *Subscription) Channel() *uint32 {
	if s.channel == nil {
		return nil
	}
	v := *s.channel
	return &v
}

func (s *Subscription) consume(payload []byte) {
	s.lineMu.Lock()
	if s.discarding {
		if i := bytes.IndexByte(payload, '\n'); i >= 0 {
			payload = payload[i+1:]
			s.discarding = false
		} else {
			s.lineMu.Unlock()
			return
		}
	}
	s.lineBuf = append(s.lineBuf, payload...)
	var lines [][]byte
	for {
		i := bytes.IndexByte(s.lineBuf, '\n')
		if i < 0 {
			break
		}
		line := s.lineBuf[:i]
		if len(line) > 0 {
			lines = append(lines, append([]byte(nil), line...))
		}
		s.lineBuf = s.lineBuf[i+1:]
	}
	overflow := len(s.lineBuf) > maxEventLineBuffer
	if overflow {
		s.lineBuf = nil
		s.discarding = true
	}
	s.lineMu.Unlock()

	if overflow {
		s.report(&Error{Code: CodeProtocol, Method: "events.subscribe", Message: "event notification too large"})
	}
	for _, line := range lines {
		s.handleLine(line)
	}
}

func (s *Subscription) consumeNotification(method string, params json.RawMessage) {
	s.applyNotification(method, params)
}

func (s *Subscription) handleLine(line []byte) {
	if len(bytes.TrimSpace(line)) == 0 {
		return
	}
	var msg wireMessage
	if err := json.Unmarshal(line, &msg); err != nil {
		s.report(&Error{Code: CodeProtocol, Method: "events.subscribe", Err: err})
		return
	}
	if msg.Method == "" {
		return
	}
	s.applyNotification(msg.Method, msg.Params)
}

func (s *Subscription) applyNotification(method string, params json.RawMessage) {
	switch method {
	case "event":
		s.applyEvent(params)
	case "heartbeat":
		// Liveness only; a heartbeat does not prove a gap.
	case "missed":
		s.applyMissed(params)
	}
}

func (s *Subscription) applyEvent(params json.RawMessage) {
	var event Event
	if err := json.Unmarshal(params, &event); err != nil {
		s.report(&Error{Code: CodeProtocol, Method: "events.subscribe", Err: err})
		return
	}
	if event.Data == nil {
		event.Data = json.RawMessage("null")
	} else {
		event.Data = append(json.RawMessage(nil), event.Data...)
	}

	var gap *GapError
	overflow := false
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return
	}
	if s.filtered {
		if event.Seq <= s.lastSeq {
			s.mu.Unlock()
			return
		}
		s.lastSeq = event.Seq
	} else {
		if event.Seq <= s.lastSeq {
			s.mu.Unlock()
			return
		}
		if event.Seq != s.lastSeq+1 {
			gap = &GapError{Expected: s.lastSeq + 1, Got: event.Seq}
		}
		s.lastSeq = event.Seq
	}
	select {
	case s.events <- event:
	default:
		select {
		case <-s.events:
		default:
		}
		select {
		case s.events <- event:
		default:
		}
		overflow = true
	}
	s.mu.Unlock()

	if gap != nil {
		s.report(gap)
	}
	if overflow {
		s.report(&Error{Code: CodeEventOverflow, Method: "events.subscribe"})
	}
}

func (s *Subscription) applyMissed(params json.RawMessage) {
	var missed struct {
		Seq       uint64     `json:"seq"`
		Terminals []Terminal `json:"terminals"`
	}
	if err := json.Unmarshal(params, &missed); err != nil {
		s.report(&Error{Code: CodeProtocol, Method: "events.subscribe", Err: err})
		return
	}
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return
	}
	s.snapshot = append([]Terminal(nil), missed.Terminals...)
	s.lastSeq = missed.Seq
	s.missed++
	s.mu.Unlock()
	s.report(&GapError{Expected: missed.Seq, Got: missed.Seq, Recovered: true})
}

func (s *Subscription) report(err error) {
	if err == nil {
		return
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return
	}
	select {
	case s.errors <- err:
	default:
	}
}

func (s *Subscription) fail(err error) {
	if err == nil {
		err = &Error{Code: CodeSubscriptionClosed, Method: "events.subscribe"}
	}
	s.report(err)
	s.closeLocal(err)
}

func (s *Subscription) closeLocal(err error) {
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return
	}
	s.closed = true
	close(s.done)
	close(s.events)
	close(s.errors)
	s.mu.Unlock()
	s.client.removeSubscription(s.channel)
}

// Close ends the subscription. It never stops a terminal.
func (s *Subscription) Close() error {
	s.mu.Lock()
	closed := s.closed
	channel := s.channel
	s.mu.Unlock()
	if closed {
		return nil
	}
	if channel != nil {
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		_ = s.client.sendFrame(ctx, *channel, KindClose, nil)
		cancel()
	}
	s.closeLocal(&Error{Code: CodeSubscriptionClosed, Method: "events.subscribe"})
	return nil
}
