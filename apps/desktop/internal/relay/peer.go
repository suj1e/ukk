package relay

import (
	"context"
	"sync"
	"sync/atomic"
	"time"

	"github.com/coder/websocket"
)

type peerRole int

const (
	roleHost peerRole = iota
	roleDevice
)

type peer struct {
	srv      *Server
	ws       *websocket.Conn
	role     peerRole
	hostID   string
	deviceID string
	send     chan []byte
	done     chan struct{}
	once     sync.Once
	lastSeen atomic.Int64
}

func newPeer(s *Server, ws *websocket.Conn, role peerRole) *peer {
	p := &peer{
		srv:  s,
		ws:   ws,
		role: role,
		send: make(chan []byte, QueueFrames),
		done: make(chan struct{}),
	}
	p.touch()
	return p
}

func (p *peer) touch() { p.lastSeen.Store(time.Now().UnixNano()) }

func (p *peer) seen() time.Time { return time.Unix(0, p.lastSeen.Load()) }

func (p *peer) enqueue(raw []byte) bool {
	select {
	case <-p.done:
		return false
	default:
	}
	select {
	case p.send <- raw:
		return true
	default:
		return false
	}
}

// terminate closes the peer exactly once. It never blocks on the close
// handshake so hub cleanup and shutdown stay prompt.
func (p *peer) terminate(code Code) {
	p.once.Do(func() {
		close(p.done)
		_ = p.ws.CloseNow()
	})
}

func (p *peer) writeFrame(f frame) error {
	raw, err := encodeFrame(f)
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(p.srv.ctx, p.srv.cfg.WriteTimeout)
	defer cancel()
	return p.ws.Write(ctx, websocket.MessageText, raw)
}

func (p *peer) sendError(err *Error) {
	if err == nil {
		return
	}
	_ = p.writeFrame(frame{T: "error", Code: string(err.Code), Message: err.Message})
	p.srv.emit(Event{
		Kind:     EventRejected,
		HostID:   p.hostID,
		DeviceID: p.deviceID,
		Code:     err.Code,
	})
}

func (p *peer) fail(err *Error) {
	p.sendError(err)
	if err != nil {
		p.terminate(err.Code)
		return
	}
	p.terminate(CodeClosed)
}

func (p *peer) writeLoop() {
	ticker := time.NewTicker(p.srv.cfg.PingInterval)
	defer ticker.Stop()
	for {
		select {
		case raw := <-p.send:
			ctx, cancel := context.WithTimeout(p.srv.ctx, p.srv.cfg.WriteTimeout)
			err := p.ws.Write(ctx, websocket.MessageText, raw)
			cancel()
			if err != nil {
				p.terminate(CodeClosed)
				return
			}
		case <-ticker.C:
			if time.Since(p.seen()) > p.srv.cfg.IdleTimeout {
				p.terminate(CodeIdleTimeout)
				return
			}
			ctx, cancel := context.WithTimeout(p.srv.ctx, p.srv.cfg.WriteTimeout)
			err := p.ws.Ping(ctx)
			cancel()
			if err != nil {
				p.terminate(CodeIdleTimeout)
				return
			}
			p.touch()
		case <-p.done:
			return
		case <-p.srv.ctx.Done():
			p.terminate(CodeClosed)
			return
		}
	}
}
