package remotescreen

import (
	"context"
	"crypto/rand"
	"encoding/binary"
	"errors"
	"fmt"
	"sync"
	"time"

	"github.com/pion/rtp"
	"github.com/pion/rtp/codecs"
	"github.com/pion/webrtc/v4"
)

const (
	// DefaultSTUNURL is the public STUN server used unless options override it.
	DefaultSTUNURL = "stun:stun.l.google.com:19302"
	// defaultMTU keeps H.264 RTP packets below common path MTUs.
	defaultMTU = 1200
)

var (
	// ErrPublisherClosed reports use after Close.
	ErrPublisherClosed = errors.New("remotescreen: media publisher closed")
	// ErrPublisherNotReady reports a frame written before an offer exists.
	ErrPublisherNotReady = errors.New("remotescreen: media publisher has no offer")
	// ErrInvalidFrame reports empty or invalid media input.
	ErrInvalidFrame = errors.New("remotescreen: invalid media frame")
	// ErrInvalidPublisherOptions reports invalid ICE/MTU configuration.
	ErrInvalidPublisherOptions = errors.New("remotescreen: invalid publisher options")
)

// TURNOptions configures an optional TURN relay for NAT traversal. Credentials
// are short-lived TURN credentials supplied by the operator, never pairing
// secrets.
type TURNOptions struct {
	URLs       []string
	Username   string
	Credential string
}

// PublisherOptions configures independent WebRTC media. STUNURLs nil selects
// DefaultSTUNURL; a non-nil empty slice disables STUN for loopback tests.
type PublisherOptions struct {
	STUNURLs []string
	TURN     *TURNOptions
	MTU      uint16
}

// Publisher sends H.264 access units over one WebRTC video track. The relay
// never carries these packets: ICE/DTLS-SRTP connects the peers directly.
type Publisher struct {
	mu      sync.Mutex
	pc      *webrtc.PeerConnection
	track   *webrtc.TrackLocalStaticRTP
	payload *codecs.H264Payloader

	mtu        uint16
	clockRate  uint32
	ssrc       uint32
	seq        uint16
	offer      *webrtc.SessionDescription
	capture    Capture
	closed     bool
	lastKeyPTS time.Duration
	haveKeyPTS bool
}

var h264Capability = webrtc.RTPCodecCapability{
	MimeType:    webrtc.MimeTypeH264,
	ClockRate:   90000,
	Channels:    0,
	SDPFmtpLine: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f",
}

// NewPublisher creates the peer connection and adds the H.264 video track.
func NewPublisher(opts PublisherOptions) (*Publisher, error) {
	servers, err := publisherICEServers(opts)
	if err != nil {
		return nil, err
	}
	mtu := opts.MTU
	if mtu == 0 {
		mtu = defaultMTU
	}
	if mtu < 576 || mtu > 1500 {
		return nil, fmt.Errorf("%w: MTU %d", ErrInvalidPublisherOptions, mtu)
	}

	pc, err := webrtc.NewPeerConnection(webrtc.Configuration{ICEServers: servers})
	if err != nil {
		return nil, fmt.Errorf("remotescreen: new peer connection: %w", err)
	}
	track, err := webrtc.NewTrackLocalStaticRTP(h264Capability, "video", "ukk-screen")
	if err != nil {
		_ = pc.Close()
		return nil, fmt.Errorf("remotescreen: new video track: %w", err)
	}
	sender, err := pc.AddTrack(track)
	if err != nil {
		_ = pc.Close()
		return nil, fmt.Errorf("remotescreen: add video track: %w", err)
	}

	// Drain RTCP (NACK/PLI/REMB) so interceptors do not block. The loop exits
	// when the peer connection closes.
	go func() {
		buf := make([]byte, 1500)
		for {
			if _, _, err := sender.Read(buf); err != nil {
				return
			}
		}
	}()

	var seed [6]byte
	if _, err := rand.Read(seed[:]); err != nil {
		_ = pc.Close()
		return nil, fmt.Errorf("remotescreen: seed RTP state: %w", err)
	}
	return &Publisher{
		pc:        pc,
		track:     track,
		payload:   &codecs.H264Payloader{},
		mtu:       mtu,
		clockRate: 90000,
		ssrc:      binary.BigEndian.Uint32(seed[:4]),
		seq:       binary.BigEndian.Uint16(seed[4:]),
	}, nil
}

func publisherICEServers(opts PublisherOptions) ([]webrtc.ICEServer, error) {
	urls := opts.STUNURLs
	if urls == nil {
		urls = []string{DefaultSTUNURL}
	}
	var servers []webrtc.ICEServer
	if len(urls) > 0 {
		if err := validateICEURLs(urls); err != nil {
			return nil, err
		}
		servers = append(servers, webrtc.ICEServer{URLs: urls})
	}
	if opts.TURN != nil {
		if len(opts.TURN.URLs) == 0 {
			return nil, fmt.Errorf("%w: TURN URLs are empty", ErrInvalidPublisherOptions)
		}
		if opts.TURN.Username == "" || opts.TURN.Credential == "" {
			return nil, fmt.Errorf("%w: TURN credentials are required", ErrInvalidPublisherOptions)
		}
		if err := validateICEURLs(opts.TURN.URLs); err != nil {
			return nil, err
		}
		servers = append(servers, webrtc.ICEServer{
			URLs:           opts.TURN.URLs,
			Username:       opts.TURN.Username,
			Credential:     opts.TURN.Credential,
			CredentialType: webrtc.ICECredentialTypePassword,
		})
	}
	return servers, nil
}

func validateICEURLs(urls []string) error {
	for _, u := range urls {
		if len(u) < 5 {
			return fmt.Errorf("%w: ICE URL %q", ErrInvalidPublisherOptions, u)
		}
		switch {
		case len(u) >= 5 && u[:5] == "stun:":
		case len(u) >= 6 && u[:6] == "stuns:":
		case len(u) >= 5 && u[:5] == "turn:":
		case len(u) >= 6 && u[:6] == "turns:":
		default:
			return fmt.Errorf("%w: ICE URL %q", ErrInvalidPublisherOptions, u)
		}
	}
	return nil
}

// CreateOffer creates the local offer, waits for ICE gathering to complete
// (host, STUN and TURN candidates included in the SDP) and returns it. The
// context bounds gathering; cancellation returns without closing the
// publisher.
func (p *Publisher) CreateOffer(ctx context.Context) (webrtc.SessionDescription, error) {
	if ctx == nil {
		ctx = context.Background()
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.closed {
		return webrtc.SessionDescription{}, ErrPublisherClosed
	}
	if p.offer != nil {
		return *p.offer, nil
	}

	gatherComplete := webrtc.GatheringCompletePromise(p.pc)
	offer, err := p.pc.CreateOffer(nil)
	if err != nil {
		return webrtc.SessionDescription{}, fmt.Errorf("remotescreen: create offer: %w", err)
	}
	if err := p.pc.SetLocalDescription(offer); err != nil {
		return webrtc.SessionDescription{}, fmt.Errorf("remotescreen: set local description: %w", err)
	}
	select {
	case <-gatherComplete:
	case <-ctx.Done():
		return webrtc.SessionDescription{}, fmt.Errorf("remotescreen: ICE gathering: %w", ctx.Err())
	}
	local := p.pc.LocalDescription()
	if local == nil {
		return webrtc.SessionDescription{}, errors.New("remotescreen: offer missing after ICE gathering")
	}
	p.offer = local
	return *local, nil
}

// AcceptAnswer applies the remote answer. The answer must already contain the
// peer's candidates (non-trickle) so no later candidate callback is required.
func (p *Publisher) AcceptAnswer(answer webrtc.SessionDescription) error {
	if answer.Type != webrtc.SDPTypeAnswer {
		return fmt.Errorf("remotescreen: expected answer SDP, got %s", answer.Type)
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.closed {
		return ErrPublisherClosed
	}
	if p.offer == nil {
		return ErrPublisherNotReady
	}
	if err := p.pc.SetRemoteDescription(answer); err != nil {
		return fmt.Errorf("remotescreen: set remote description: %w", err)
	}
	return nil
}

// WriteFrame packetizes one Annex-B access unit with an RTP timestamp derived
// from Frame.PTS. Keyframes are detected from the IDR NAL unit when the caller
// did not mark the frame.
func (p *Publisher) WriteFrame(frame Frame) error {
	if len(frame.Data) == 0 {
		return fmt.Errorf("%w: empty data", ErrInvalidFrame)
	}
	if frame.PTS < 0 {
		return fmt.Errorf("%w: negative PTS", ErrInvalidFrame)
	}

	p.mu.Lock()
	defer p.mu.Unlock()
	if p.closed {
		return ErrPublisherClosed
	}
	if p.offer == nil {
		return ErrPublisherNotReady
	}

	keyframe := frame.Keyframe || annexBContainsIDR(frame.Data)
	if keyframe {
		p.lastKeyPTS = frame.PTS
		p.haveKeyPTS = true
	}

	timestamp := rtpTimestamp(frame.PTS, p.clockRate)
	payloads := p.payload.Payload(p.mtu, frame.Data)
	if len(payloads) == 0 {
		return nil
	}
	for i, payload := range payloads {
		packet := &rtp.Packet{
			Header: rtp.Header{
				Version:        2,
				PayloadType:    96, // rewritten to the negotiated payload type
				SequenceNumber: p.seq,
				Timestamp:      timestamp,
				SSRC:           p.ssrc,
				Marker:         i == len(payloads)-1,
			},
			Payload: payload,
		}
		p.seq++
		if err := p.track.WriteRTP(packet); err != nil {
			return fmt.Errorf("remotescreen: write RTP: %w", err)
		}
	}
	return nil
}

// LastKeyframePTS returns the PTS of the last detected keyframe and whether
// one has been written.
func (p *Publisher) LastKeyframePTS() (time.Duration, bool) {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.lastKeyPTS, p.haveKeyPTS
}

// ConnectionState reports the current peer connection state.
func (p *Publisher) ConnectionState() webrtc.PeerConnectionState {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.pc == nil {
		return webrtc.PeerConnectionStateClosed
	}
	return p.pc.ConnectionState()
}

// OnConnectionStateChange registers a state callback.
func (p *Publisher) OnConnectionStateChange(f func(webrtc.PeerConnectionState)) {
	p.mu.Lock()
	pc := p.pc
	p.mu.Unlock()
	if pc != nil {
		pc.OnConnectionStateChange(f)
	}
}

// AttachCapture transfers shutdown ownership of a live capture to the
// publisher. Stream uses this; callers that start capture directly may use it
// too so Publisher.Close always stops capture and frees the encoder.
func (p *Publisher) AttachCapture(capture Capture) error {
	if capture == nil {
		return errors.New("remotescreen: nil capture")
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.closed {
		return ErrPublisherClosed
	}
	if p.capture != nil {
		return errors.New("remotescreen: capture already attached")
	}
	p.capture = capture
	return nil
}

// Close stops any attached capture, closes the peer connection and releases
// media resources.
func (p *Publisher) Close() error {
	p.mu.Lock()
	if p.closed {
		p.mu.Unlock()
		return nil
	}
	p.closed = true
	pc := p.pc
	capture := p.capture
	p.capture = nil
	p.track = nil
	p.mu.Unlock()

	var errs []error
	if capture != nil {
		if err := capture.Stop(); err != nil {
			errs = append(errs, fmt.Errorf("remotescreen: stop attached capture: %w", err))
		}
	}
	if pc != nil {
		if err := pc.Close(); err != nil {
			errs = append(errs, err)
		}
	}
	return errors.Join(errs...)
}

func rtpTimestamp(pts time.Duration, clockRate uint32) uint32 {
	if pts <= 0 || clockRate == 0 {
		return 0
	}
	seconds := uint64(pts / time.Second)
	nanos := uint64(pts % time.Second)
	ticks := seconds*uint64(clockRate) + (nanos*uint64(clockRate)+uint64(time.Second)/2)/uint64(time.Second)
	return uint32(ticks)
}

// annexBContainsIDR reports whether an Annex-B access unit carries an IDR
// slice (NAL type 5).
func annexBContainsIDR(data []byte) bool {
	from := 0
	foundStart := false
	for {
		_, naluStart, ok := findStartCode(data, from)
		if !ok {
			break
		}
		foundStart = true
		if naluStart < len(data) && (data[naluStart]&0x1f) == nalTypeIDRSlice {
			return true
		}
		from = naluStart
	}
	if !foundStart && len(data) > 0 {
		return (data[0] & 0x1f) == nalTypeIDRSlice
	}
	return false
}
