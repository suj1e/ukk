package hc

import (
	"bufio"
	"context"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

// ProtocolGeneration is the only framed protocol generation this client
// negotiates.
const ProtocolGeneration uint32 = 1

const (
	defaultBinary      = "hc"
	defaultStartupWait = 10 * time.Second
	maxHelloLine       = 1 << 20
	maxRequestJSON     = 64 << 10
	maxPendingCalls    = 128
	maxQueuedBytes     = 8 << 20
	maxStderrBytes     = 64 << 10
	writeQueueDepth    = 128
	reapWait           = 2 * time.Second
)

// Client is one `hc connect` child and its JSON-RPC connection.
type Client struct {
	cmd    *exec.Cmd
	stdin  io.WriteCloser
	stdout io.ReadCloser
	stderr *tailBuffer

	startupWait time.Duration
	callWait    time.Duration

	outCh    chan outFrame
	outMu    sync.Mutex
	outBytes int64

	writeDone chan struct{}
	readDone  chan struct{}
	waitDone  chan struct{}
	done      chan struct{}

	termOnce sync.Once
	termMu   sync.Mutex
	termErr  error

	killOnce  sync.Once
	closeOnce sync.Once
	waitMu    sync.Mutex
	waitErr   error

	mu            sync.Mutex
	pending       map[uint64]*pendingCall
	attachments   map[uint32]*Attachment
	subscriptions map[uint32]*Subscription
	plainSub      *Subscription
	nextID        atomic.Uint64

	hello HelloResult
}

type outFrame struct {
	data []byte
}

type pendingCall struct {
	ch        chan callResult
	prepare   func(json.RawMessage) (func(), error)
	mu        sync.Mutex
	abandoned bool
}

type callResult struct {
	result  json.RawMessage
	err     error
	cleanup func()
}

type wireRequest struct {
	JSONRPC string `json:"jsonrpc"`
	ID      uint64 `json:"id"`
	Method  string `json:"method"`
	Params  any    `json:"params,omitempty"`
}

type wireMessage struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      json.RawMessage `json:"id"`
	Method  string          `json:"method"`
	Params  json.RawMessage `json:"params"`
	Result  json.RawMessage `json:"result"`
	Error   *wireError      `json:"error"`
}

type wireError struct {
	Code    int             `json:"code"`
	Message string          `json:"message"`
	Data    json.RawMessage `json:"data"`
}

type tailBuffer struct {
	mu    sync.Mutex
	limit int
	buf   []byte
}

func (b *tailBuffer) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	if len(p) >= b.limit {
		b.buf = append(b.buf[:0], p[len(p)-b.limit:]...)
		return len(p), nil
	}
	b.buf = append(b.buf, p...)
	if len(b.buf) > b.limit {
		copy(b.buf, b.buf[len(b.buf)-b.limit:])
		b.buf = b.buf[:b.limit]
	}
	return len(p), nil
}

// Connect starts `hc connect` and completes the hello handshake.
//
// Cancelling ctx kills and reaps the child for the rest of the connection's
// lifetime, just as Close does. The child is always killed and reaped on a
// failed handshake.
func Connect(ctx context.Context, opts Options) (*Client, error) {
	if ctx == nil {
		ctx = context.Background()
	}
	binary := opts.Binary
	if binary == "" {
		binary = defaultBinary
	}
	args := make([]string, 0, 1+len(opts.Args))
	args = append(args, "connect")
	args = append(args, opts.Args...)

	cmd := exec.CommandContext(ctx, binary, args...)
	cmd.Env = append(os.Environ(), opts.Env...)
	stdin, err := cmd.StdinPipe()
	if err != nil {
		return nil, &Error{Code: CodeSpawn, Err: err}
	}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		_ = stdin.Close()
		return nil, &Error{Code: CodeSpawn, Err: err}
	}
	stderr := &tailBuffer{limit: maxStderrBytes}
	cmd.Stderr = stderr

	startup := opts.Timeout
	if startup <= 0 {
		startup = defaultStartupWait
	}
	c := &Client{
		cmd:           cmd,
		stdin:         stdin,
		stdout:        stdout,
		stderr:        stderr,
		startupWait:   startup,
		callWait:      opts.Timeout,
		outCh:         make(chan outFrame, writeQueueDepth),
		writeDone:     make(chan struct{}),
		readDone:      make(chan struct{}),
		waitDone:      make(chan struct{}),
		done:          make(chan struct{}),
		pending:       make(map[uint64]*pendingCall),
		attachments:   make(map[uint32]*Attachment),
		subscriptions: make(map[uint32]*Subscription),
	}
	if err := cmd.Start(); err != nil {
		_ = stdin.Close()
		_ = stdout.Close()
		code := CodeSpawn
		if ctx.Err() != nil {
			code = CodeCanceled
		}
		return nil, &Error{Code: code, Err: err}
	}
	go c.waitLoop()
	go c.writeLoop()

	br := bufio.NewReaderSize(stdout, 32<<10)
	if err := c.handshake(ctx, br); err != nil {
		c.terminate(err)
		c.waitReaped(reapWait)
		_ = c.stdout.Close()
		waitUntil(c.writeDone, time.Second)
		return nil, err
	}
	go c.readLoop(br)
	return c, nil
}

// Hello is the negotiated handshake result.
func (c *Client) Hello() HelloResult { return c.hello }

// Done is closed when the connection can no longer be used.
func (c *Client) Done() <-chan struct{} { return c.done }

// Err is the connection's terminal error, if any.
func (c *Client) Err() error {
	c.termMu.Lock()
	defer c.termMu.Unlock()
	return c.termErr
}

// Close kills and reaps the child and unblocks every pending call, read and
// subscription. It never stops a core terminal.
func (c *Client) Close() error {
	c.closeOnce.Do(func() {
		c.terminate(&Error{Code: CodeClosed})
		c.waitReaped(reapWait)
		_ = c.stdout.Close()
		waitUntil(c.readDone, time.Second)
		waitUntil(c.writeDone, time.Second)
	})
	return nil
}

func (c *Client) handshake(ctx context.Context, br *bufio.Reader) error {
	params := struct {
		Framed     bool   `json:"framed"`
		Generation uint32 `json:"generation"`
	}{Framed: true, Generation: ProtocolGeneration}
	body, err := json.Marshal(wireRequest{JSONRPC: "2.0", ID: 1, Method: "hello", Params: params})
	if err != nil {
		return &Error{Code: CodeProtocol, Method: "hello", Err: err}
	}
	if err := c.enqueue(ctx, append(body, '\n')); err != nil {
		return err
	}

	type lineResult struct {
		line []byte
		err  error
	}
	lines := make(chan lineResult, 1)
	go func() {
		line, err := readLineLimited(br, maxHelloLine)
		lines <- lineResult{line: line, err: err}
	}()

	timer := time.NewTimer(c.startupWait)
	defer timer.Stop()
	var res lineResult
	select {
	case res = <-lines:
	case <-ctx.Done():
		err := &Error{Code: CodeCanceled, Method: "hello", Err: ctx.Err()}
		c.terminate(err)
		c.waitReaped(reapWait)
		_ = c.stdout.Close()
		<-lines
		return err
	case <-timer.C:
		err := &Error{Code: CodeTimeout, Method: "hello"}
		c.terminate(err)
		c.waitReaped(reapWait)
		_ = c.stdout.Close()
		<-lines
		return err
	case <-c.done:
		return c.connectionError()
	}
	if res.err != nil {
		return &Error{Code: CodeConnection, Method: "hello", Err: res.err}
	}

	var msg wireMessage
	if err := json.Unmarshal(res.line, &msg); err != nil {
		return &Error{Code: CodeProtocol, Method: "hello", Err: err}
	}
	if msg.Error != nil {
		return c.rpcError("hello", msg.Error)
	}
	var hello HelloResult
	if err := json.Unmarshal(msg.Result, &hello); err != nil {
		return &Error{Code: CodeProtocol, Method: "hello", Err: err}
	}
	if !hello.Framed || hello.Generation != ProtocolGeneration {
		return &Error{
			Code:    CodeHandshake,
			Method:  "hello",
			Message: fmt.Sprintf("framed=%t generation=%d", hello.Framed, hello.Generation),
		}
	}
	if hello.Version == "" || hello.Core.ID == "" {
		return &Error{Code: CodeProtocol, Method: "hello", Message: "incomplete hello"}
	}
	c.hello = hello
	c.nextID.Store(1)
	return nil
}

func (c *Client) waitLoop() {
	err := c.cmd.Wait()
	c.waitMu.Lock()
	c.waitErr = err
	c.waitMu.Unlock()
	close(c.waitDone)
	c.terminate(&Error{Code: CodeConnection, Err: err})
}

func (c *Client) writeLoop() {
	defer close(c.writeDone)
	for {
		select {
		case f := <-c.outCh:
			err := writeAll(c.stdin, f.data)
			c.outMu.Lock()
			c.outBytes -= int64(len(f.data))
			c.outMu.Unlock()
			if err != nil {
				c.terminate(&Error{Code: CodeConnection, Err: err})
				return
			}
		case <-c.done:
			return
		}
	}
}

func (c *Client) readLoop(br *bufio.Reader) {
	defer close(c.readDone)
	for {
		frame, err := ReadFrame(br)
		if err != nil {
			var terminal error
			switch {
			case errors.Is(err, io.EOF):
				terminal = c.connectionError()
			case errors.Is(err, ErrFrameTooLarge):
				terminal = &Error{Code: CodePayloadTooLarge, Err: err}
			case errors.Is(err, ErrProtocol):
				terminal = &Error{Code: CodeProtocol, Err: err}
			default:
				terminal = &Error{Code: CodeConnection, Err: err}
			}
			c.terminate(terminal)
			return
		}
		c.handleFrame(frame)
	}
}

func (c *Client) handleFrame(frame Frame) {
	if frame.Channel == 0 {
		if frame.Kind == KindData {
			c.handleControlJSON(frame.Payload)
		}
		// Unknown channel-0 kinds are skipped, never fatal.
		return
	}

	c.mu.Lock()
	att := c.attachments[frame.Channel]
	sub := c.subscriptions[frame.Channel]
	c.mu.Unlock()

	switch frame.Kind {
	case KindData:
		switch {
		case att != nil:
			att.push(frame.Payload)
		case sub != nil:
			sub.consume(frame.Payload)
			c.sendCredit(frame.Channel, len(frame.Payload))
		}
	case KindCredit:
		if att != nil {
			att.credit(uint64(binary.LittleEndian.Uint32(pad4(frame.Payload))))
		}
	case KindClose:
		switch {
		case att != nil:
			att.handleCoreClose(frame.Payload)
		case sub != nil:
			sub.fail(&Error{Code: CodeSubscriptionClosed, Method: "events.subscribe"})
		}
	case KindResize:
		if att != nil {
			att.handleCoreResize(frame.Payload)
		}
	case KindFacts:
		if att != nil {
			att.handleFacts(frame.Payload)
		}
	default:
		// Unknown kinds are skipped, not fatal.
	}
}

func pad4(p []byte) []byte {
	if len(p) >= 4 {
		return p
	}
	return []byte{0, 0, 0, 0}
}

func (c *Client) handleControlJSON(payload []byte) {
	var msg wireMessage
	if err := json.Unmarshal(payload, &msg); err != nil {
		c.terminate(&Error{Code: CodeProtocol, Err: err})
		return
	}
	if msg.Method != "" {
		c.handleNotification(msg.Method, msg.Params)
		return
	}
	id, ok := parseMessageID(msg.ID)
	if !ok {
		return
	}
	if msg.Error != nil {
		c.handleResponse(id, nil, c.rpcError("", msg.Error))
		return
	}
	c.handleResponse(id, msg.Result, nil)
}

func parseMessageID(raw json.RawMessage) (uint64, bool) {
	text := strings.TrimSpace(string(raw))
	if text == "" || text == "null" {
		return 0, false
	}
	if strings.HasPrefix(text, `"`) {
		var s string
		if err := json.Unmarshal(raw, &s); err != nil {
			return 0, false
		}
		text = s
	}
	var id uint64
	if err := json.Unmarshal([]byte(text), &id); err != nil {
		return 0, false
	}
	return id, true
}

func (c *Client) handleResponse(id uint64, result json.RawMessage, callErr error) {
	c.mu.Lock()
	pc := c.pending[id]
	delete(c.pending, id)
	c.mu.Unlock()
	if pc == nil {
		return
	}
	if callErr != nil {
		deliver(pc, callResult{err: callErr})
		return
	}
	if pc.prepare == nil {
		deliver(pc, callResult{result: result})
		return
	}
	cleanup, err := pc.prepare(result)
	if err != nil {
		if cleanup != nil {
			cleanup()
		}
		deliver(pc, callResult{err: err})
		return
	}
	deliver(pc, callResult{result: result, cleanup: cleanup})
}

func deliver(pc *pendingCall, res callResult) {
	pc.mu.Lock()
	defer pc.mu.Unlock()
	if pc.abandoned {
		if res.cleanup != nil {
			res.cleanup()
		}
		return
	}
	select {
	case pc.ch <- res:
	default:
		if res.cleanup != nil {
			res.cleanup()
		}
	}
}

func (c *Client) abandon(id uint64, pc *pendingCall) {
	c.mu.Lock()
	if c.pending[id] == pc {
		delete(c.pending, id)
	}
	c.mu.Unlock()

	pc.mu.Lock()
	pc.abandoned = true
	pc.mu.Unlock()

	select {
	case res := <-pc.ch:
		if res.cleanup != nil {
			res.cleanup()
		}
	default:
	}
}

func (c *Client) handleNotification(method string, params json.RawMessage) {
	switch method {
	case "event", "heartbeat", "missed":
		c.mu.Lock()
		sub := c.plainSub
		c.mu.Unlock()
		if sub != nil {
			sub.consumeNotification(method, params)
		}
	}
}

func (c *Client) call(ctx context.Context, method string, params, out any, prepare func(json.RawMessage) (func(), error)) error {
	return c.callWithLimit(ctx, method, params, out, prepare, maxRequestJSON)
}

// callWithLimit is call with an explicit request-body bound; documents are
// larger than ordinary RPC parameters but still bounded.
func (c *Client) callWithLimit(ctx context.Context, method string, params, out any, prepare func(json.RawMessage) (func(), error), limit int) error {
	if ctx == nil {
		ctx = context.Background()
	}
	id := c.nextID.Add(1)
	body, err := json.Marshal(wireRequest{JSONRPC: "2.0", ID: id, Method: method, Params: params})
	if err != nil {
		return &Error{Code: CodeProtocol, Method: method, Err: err}
	}
	if len(body) > limit {
		return &Error{Code: CodeRequestTooLarge, Method: method}
	}

	select {
	case <-c.done:
		return c.connectionError()
	default:
	}

	pc := &pendingCall{ch: make(chan callResult, 1), prepare: prepare}
	c.mu.Lock()
	if len(c.pending) >= maxPendingCalls {
		c.mu.Unlock()
		return &Error{Code: CodeTooManyPending, Method: method}
	}
	c.pending[id] = pc
	c.mu.Unlock()

	waitCtx := ctx
	var cancel context.CancelFunc
	if _, ok := ctx.Deadline(); !ok && c.callWait > 0 {
		waitCtx, cancel = context.WithTimeout(ctx, c.callWait)
		defer cancel()
	}
	if err := c.sendFrame(waitCtx, 0, KindData, body); err != nil {
		c.abandon(id, pc)
		return err
	}

	select {
	case res := <-pc.ch:
		if res.err != nil {
			return res.err
		}
		if out != nil {
			if len(res.result) == 0 {
				return &Error{Code: CodeProtocol, Method: method, Message: "empty result"}
			}
			if err := json.Unmarshal(res.result, out); err != nil {
				return &Error{Code: CodeProtocol, Method: method, Err: err}
			}
		}
		return nil
	case <-waitCtx.Done():
		c.abandon(id, pc)
		if errors.Is(waitCtx.Err(), context.DeadlineExceeded) {
			return &Error{Code: CodeTimeout, Method: method, Err: waitCtx.Err()}
		}
		return &Error{Code: CodeCanceled, Method: method, Err: waitCtx.Err()}
	case <-c.done:
		c.abandon(id, pc)
		return c.connectionError()
	}
}

func (c *Client) rpcError(method string, we *wireError) *Error {
	var data struct {
		Kind string `json:"kind"`
		Hint string `json:"hint"`
	}
	_ = json.Unmarshal(we.Data, &data)
	code := CodeRPC
	if data.Kind == "unsupported_generation" {
		code = CodeUnsupported
	}
	// The server's free-form message is deliberately not stored: it can
	// quote terminal contents or other sensitive data. Callers branch on the
	// stable kind instead.
	return &Error{Code: code, Method: method, Kind: data.Kind, RPCCode: we.Code}
}

func (c *Client) sendFrame(ctx context.Context, channel uint32, kind uint8, payload []byte) error {
	encoded, err := EncodeFrame(channel, kind, payload)
	if err != nil {
		return &Error{Code: CodePayloadTooLarge, Err: err}
	}
	return c.enqueue(ctx, encoded)
}

func (c *Client) sendCredit(channel uint32, n int) {
	if n <= 0 {
		return
	}
	var payload [4]byte
	binary.LittleEndian.PutUint32(payload[:], uint32(n))
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	_ = c.sendFrame(ctx, channel, KindCredit, payload[:])
}

func (c *Client) enqueue(ctx context.Context, data []byte) error {
	if ctx == nil {
		ctx = context.Background()
	}
	select {
	case <-c.done:
		return c.connectionError()
	default:
	}
	c.outMu.Lock()
	if c.outBytes+int64(len(data)) > maxQueuedBytes {
		c.outMu.Unlock()
		return &Error{Code: CodeBackpressure}
	}
	c.outBytes += int64(len(data))
	c.outMu.Unlock()

	select {
	case c.outCh <- outFrame{data: data}:
		return nil
	case <-c.done:
		c.outMu.Lock()
		c.outBytes -= int64(len(data))
		c.outMu.Unlock()
		return c.connectionError()
	case <-ctx.Done():
		c.outMu.Lock()
		c.outBytes -= int64(len(data))
		c.outMu.Unlock()
		if errors.Is(ctx.Err(), context.DeadlineExceeded) {
			return &Error{Code: CodeTimeout, Err: ctx.Err()}
		}
		return &Error{Code: CodeCanceled, Err: ctx.Err()}
	}
}

func (c *Client) terminate(err error) {
	if err == nil {
		err = &Error{Code: CodeClosed}
	}
	c.termOnce.Do(func() {
		c.termMu.Lock()
		c.termErr = err
		c.termMu.Unlock()
		close(c.done)
		_ = c.stdin.Close()
		c.killProcess()
		c.failAll(err)
	})
}

func (c *Client) killProcess() {
	c.killOnce.Do(func() {
		if c.cmd.Process != nil {
			_ = c.cmd.Process.Kill()
		}
	})
}

func (c *Client) failAll(err error) {
	c.mu.Lock()
	atts := make([]*Attachment, 0, len(c.attachments))
	for _, a := range c.attachments {
		atts = append(atts, a)
	}
	subs := make([]*Subscription, 0, len(c.subscriptions)+1)
	for _, s := range c.subscriptions {
		subs = append(subs, s)
	}
	if c.plainSub != nil {
		subs = append(subs, c.plainSub)
	}
	pend := make([]*pendingCall, 0, len(c.pending))
	for _, pc := range c.pending {
		pend = append(pend, pc)
	}
	c.attachments = make(map[uint32]*Attachment)
	c.subscriptions = make(map[uint32]*Subscription)
	c.plainSub = nil
	c.pending = make(map[uint64]*pendingCall)
	c.mu.Unlock()

	for _, a := range atts {
		a.shutdown(err, nil)
	}
	for _, s := range subs {
		s.fail(err)
	}
	for _, pc := range pend {
		deliver(pc, callResult{err: err})
	}
}

func (c *Client) connectionError() error {
	if err := c.Err(); err != nil {
		return err
	}
	return &Error{Code: CodeConnection}
}

func (c *Client) waitReaped(limit time.Duration) {
	select {
	case <-c.waitDone:
		return
	case <-time.After(limit):
	}
	c.killProcess()
	select {
	case <-c.waitDone:
	case <-time.After(limit):
	}
}

func (c *Client) removeAttachment(channel uint32) {
	c.mu.Lock()
	delete(c.attachments, channel)
	c.mu.Unlock()
}

func (c *Client) removeSubscription(channel *uint32) {
	c.mu.Lock()
	if channel == nil {
		c.plainSub = nil
	} else {
		delete(c.subscriptions, *channel)
	}
	c.mu.Unlock()
}

func readLineLimited(br *bufio.Reader, limit int) ([]byte, error) {
	var out []byte
	for {
		frag, err := br.ReadSlice('\n')
		if len(out)+len(frag) > limit {
			return nil, &Error{Code: CodeProtocol, Message: "handshake line too large"}
		}
		out = append(out, frag...)
		if err == nil {
			return out, nil
		}
		if errors.Is(err, bufio.ErrBufferFull) {
			continue
		}
		return out, err
	}
}

func writeAll(w io.Writer, data []byte) error {
	for len(data) > 0 {
		n, err := w.Write(data)
		if err != nil {
			return err
		}
		if n == 0 {
			return io.ErrShortWrite
		}
		data = data[n:]
	}
	return nil
}

func waitUntil(ch <-chan struct{}, limit time.Duration) {
	select {
	case <-ch:
	case <-time.After(limit):
	}
}

// TerminalList returns live terminals (all with All) filtered by labels.
func (c *Client) TerminalList(ctx context.Context, opts TerminalListOptions) (*ListResult, error) {
	var out ListResult
	if err := c.call(ctx, "terminal.list", opts, &out, nil); err != nil {
		return nil, err
	}
	return &out, nil
}

// TerminalGet returns one terminal's facts, live or finished.
func (c *Client) TerminalGet(ctx context.Context, terminal, caller string) (*Terminal, error) {
	params := struct {
		Terminal string `json:"terminal"`
		Caller   string `json:"caller,omitempty"`
	}{Terminal: terminal, Caller: caller}
	var out Terminal
	if err := c.call(ctx, "terminal.get", params, &out, nil); err != nil {
		return nil, err
	}
	return &out, nil
}

// TerminalRun starts a program in a new terminal.
func (c *Client) TerminalRun(ctx context.Context, opts RunOptions) (*RunResult, error) {
	var out RunResult
	if err := c.call(ctx, "terminal.run", opts, &out, nil); err != nil {
		return nil, err
	}
	return &out, nil
}

// Send pastes text and submits it once the terminal is quiet.
func (c *Client) Send(ctx context.Context, opts SendOptions) (*SendResult, error) {
	var out SendResult
	if err := c.call(ctx, "terminal.send", opts, &out, nil); err != nil {
		return nil, err
	}
	return &out, nil
}

// Keys presses named keys.
func (c *Client) Keys(ctx context.Context, terminal string, keys []string, caller string) (*KeysResult, error) {
	if keys == nil {
		keys = []string{}
	}
	params := struct {
		Terminal string   `json:"terminal"`
		Keys     []string `json:"keys"`
		Caller   string   `json:"caller,omitempty"`
	}{Terminal: terminal, Keys: keys, Caller: caller}
	var out KeysResult
	if err := c.call(ctx, "terminal.keys", params, &out, nil); err != nil {
		return nil, err
	}
	return &out, nil
}

// Interrupt presses the stop key and reports the observed outcome.
func (c *Client) Interrupt(ctx context.Context, opts InterruptOptions) (*InterruptResult, error) {
	var out InterruptResult
	if err := c.call(ctx, "terminal.interrupt", opts, &out, nil); err != nil {
		return nil, err
	}
	return &out, nil
}

// Kill ends the program and returns the observed terminal record.
func (c *Client) Kill(ctx context.Context, terminal, caller string) (*Terminal, error) {
	params := struct {
		Terminal string `json:"terminal"`
		Caller   string `json:"caller,omitempty"`
	}{Terminal: terminal, Caller: caller}
	var out Terminal
	if err := c.call(ctx, "terminal.kill", params, &out, nil); err != nil {
		return nil, err
	}
	return &out, nil
}

// Rm removes a finished terminal.
func (c *Client) Rm(ctx context.Context, terminal, caller string) (*RemoveResult, error) {
	params := struct {
		Terminal string `json:"terminal"`
		Caller   string `json:"caller,omitempty"`
	}{Terminal: terminal, Caller: caller}
	var out RemoveResult
	if err := c.call(ctx, "terminal.rm", params, &out, nil); err != nil {
		return nil, err
	}
	return &out, nil
}

// Keep sets whether a live terminal restarts as a shell after loss.
func (c *Client) Keep(ctx context.Context, terminal string, keep bool) (*Terminal, error) {
	params := struct {
		Terminal string `json:"terminal"`
		Keep     bool   `json:"keep"`
	}{Terminal: terminal, Keep: keep}
	var out Terminal
	if err := c.call(ctx, "terminal.keep", params, &out, nil); err != nil {
		return nil, err
	}
	return &out, nil
}

// Read returns the screen plus lines of history as text.
func (c *Client) Read(ctx context.Context, terminal string, lines uint, caller string) (*ReadResult, error) {
	params := struct {
		Terminal string `json:"terminal"`
		Lines    uint   `json:"lines"`
		Caller   string `json:"caller,omitempty"`
	}{Terminal: terminal, Lines: lines, Caller: caller}
	var out ReadResult
	if err := c.call(ctx, "terminal.read", params, &out, nil); err != nil {
		return nil, err
	}
	return &out, nil
}

// ServerStatus returns the core's identity, version and terminal counts.
func (c *Client) ServerStatus(ctx context.Context) (*StatusResult, error) {
	var out StatusResult
	if err := c.call(ctx, "server.status", nil, &out, nil); err != nil {
		return nil, err
	}
	return &out, nil
}
