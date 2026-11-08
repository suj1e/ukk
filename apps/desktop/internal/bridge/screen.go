package bridge

import (
	"context"
	"errors"
	"fmt"
	"image"
	"image/png"
	"log/slog"
	"os"
	"os/exec"
	"runtime"
	"sync"

	"github.com/pion/webrtc/v4"
	"github.com/suj1e/ukk/apps/desktop/internal/remotescreen"
)

// ScreenManager owns at most one live screen share per Session. Capture and
// input are separate grants; stopping or revoking releases both.
type ScreenManager struct {
	cfg Config
	log *slog.Logger

	mu       sync.Mutex
	sessions map[string]*screenShare
}

type screenShare struct {
	sessionID string
	capture   remotescreen.Capture
	media     *remotescreen.Publisher
	lifecycle *remotescreen.Session
	auth      *remotescreen.GrantAuthorizer
	cancel    context.CancelFunc
	control   bool
}

// NewScreenManager creates the manager.
func NewScreenManager(cfg Config, log *slog.Logger) *ScreenManager {
	if log == nil {
		log = slog.Default()
	}
	return &ScreenManager{cfg: cfg, log: log, sessions: map[string]*screenShare{}}
}

// Start authorizes screen.view, starts capture and returns the WebRTC offer.
func (m *ScreenManager) Start(ctx context.Context, params ScreenStartParams) (ScreenSession, error) {
	if params.SessionID == "" {
		return ScreenSession{}, &Error{Code: "invalid_params", Message: "session_id required"}
	}
	if !params.View {
		return ScreenSession{}, &Error{Code: "invalid_params", Message: "screen view would not be granted"}
	}
	m.mu.Lock()
	if _, exists := m.sessions[params.SessionID]; exists {
		m.mu.Unlock()
		return ScreenSession{}, &Error{Code: "screen_active", Message: "a screen share is already active in this Session"}
	}
	m.mu.Unlock()

	auth := remotescreen.NewGrantAuthorizer()
	if err := auth.Grant(remotescreen.ActionScreenView); err != nil {
		return ScreenSession{}, err
	}
	capture := remotescreen.NewFFmpegCapture()
	lifecycle := remotescreen.NewSession(auth)
	startCtx, cancel := context.WithCancel(context.Background())
	if err := lifecycle.Start(startCtx, capture, remotescreen.Options{FPS: 30, MaxWidth: 1920, Bitrate: 4_000_000}); err != nil {
		cancel()
		return ScreenSession{}, &Error{Code: "capture_unavailable", Message: err.Error()}
	}

	media, err := remotescreen.NewPublisher(remotescreen.PublisherOptions{
		STUNURLs: m.stunURLs(),
		TURN:     m.turn(),
	})
	if err != nil {
		cancel()
		_ = lifecycle.Stop()
		return ScreenSession{}, &Error{Code: "media_failed", Message: err.Error()}
	}
	offer, err := media.CreateOffer(ctx)
	if err != nil {
		cancel()
		_ = media.Close()
		_ = lifecycle.Stop()
		return ScreenSession{}, &Error{Code: "media_failed", Message: err.Error()}
	}

	share := &screenShare{
		sessionID: params.SessionID, capture: capture, media: media,
		lifecycle: lifecycle, auth: auth, cancel: cancel, control: params.Control,
	}
	if params.Control {
		if err := auth.Grant(remotescreen.ActionScreenControl); err != nil {
			m.shutdownShare(share)
			return ScreenSession{}, err
		}
		input, err := remotescreen.NewPlatformInput()
		if err != nil {
			m.shutdownShare(share)
			return ScreenSession{}, &Error{Code: "input_unavailable", Message: err.Error()}
		}
		gated, err := remotescreen.NewGatedInput(auth, input, displayBounds())
		if err != nil {
			m.shutdownShare(share)
			return ScreenSession{}, &Error{Code: "input_unavailable", Message: err.Error()}
		}
		if err := lifecycle.SetInput(gated); err != nil {
			m.shutdownShare(share)
			return ScreenSession{}, &Error{Code: "input_unavailable", Message: err.Error()}
		}
	}

	m.mu.Lock()
	m.sessions[params.SessionID] = share
	m.mu.Unlock()

	go m.pump(share)
	m.log.Info("remote screen started", "session", params.SessionID, "control", params.Control)
	return ScreenSession{
		SessionID: params.SessionID, SDP: offer.SDP,
		ICEServers: m.iceServers(), View: true, Control: params.Control,
	}, nil
}

func (m *ScreenManager) pump(share *screenShare) {
	for frame := range share.capture.Frames() {
		if err := share.media.WriteFrame(frame); err != nil && !errors.Is(err, remotescreen.ErrPublisherClosed) {
			m.log.Warn("screen frame dropped", "error", err)
		}
	}
}

// Answer completes media negotiation.
func (m *ScreenManager) Answer(_ context.Context, params ScreenAnswerParams) error {
	share := m.share(params.SessionID)
	if share == nil {
		return &Error{Code: "screen_not_active", Message: "no screen share in this Session"}
	}
	if err := share.media.AcceptAnswer(webrtc.SessionDescription{
		Type: webrtc.SDPTypeAnswer, SDP: params.SDP,
	}); err != nil {
		return &Error{Code: "media_failed", Message: err.Error()}
	}
	return nil
}

// Stop ends capture and input for a Session.
func (m *ScreenManager) Stop(_ context.Context, sessionID string) error {
	m.mu.Lock()
	share := m.sessions[sessionID]
	delete(m.sessions, sessionID)
	m.mu.Unlock()
	if share == nil {
		return nil
	}
	m.shutdownShare(share)
	return nil
}

// Input injects authorized input events.
func (m *ScreenManager) Input(_ context.Context, params ScreenInputParams) error {
	share := m.share(params.SessionID)
	if share == nil {
		return &Error{Code: "screen_not_active", Message: "no screen share in this Session"}
	}
	if !share.control {
		return &Error{Code: "forbidden", Message: "screen control was not granted"}
	}
	input := share.lifecycle.Input()
	if input == nil {
		return &Error{Code: "input_unavailable", Message: "input injection is unavailable"}
	}
	for _, event := range params.Events {
		if err := inject(input, event); err != nil {
			return &Error{Code: "input_failed", Message: err.Error()}
		}
	}
	return nil
}

func inject(input remotescreen.Input, event InputEvent) error {
	switch event.Kind {
	case "pointer":
		return input.Pointer(remotescreen.PointerMove, int(event.X), int(event.Y), remotescreen.ButtonNone)
	case "down", "up":
		kind := remotescreen.PointerDown
		if event.Kind == "up" {
			kind = remotescreen.PointerUp
		}
		button := remotescreen.ButtonLeft
		switch event.Button {
		case 2:
			button = remotescreen.ButtonRight
		case 3:
			button = remotescreen.ButtonMiddle
		}
		return input.Pointer(kind, int(event.X), int(event.Y), button)
	case "wheel":
		return input.Wheel(int(event.DeltaX), int(event.DeltaY))
	case "key":
		if event.Code == "" {
			return errors.New("key code required")
		}
		return input.Key(remotescreen.KeyCode(event.Code), event.Down)
	default:
		return fmt.Errorf("unknown input event %q", event.Kind)
	}
}

func (m *ScreenManager) share(sessionID string) *screenShare {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.sessions[sessionID]
}

func (m *ScreenManager) shutdownShare(share *screenShare) {
	if share.cancel != nil {
		share.cancel()
	}
	if share.lifecycle != nil {
		_ = share.lifecycle.Stop()
	}
	if share.media != nil {
		_ = share.media.Close()
	}
}

func (m *ScreenManager) stunURLs() []string {
	if len(m.cfg.STUNURLs) == 0 {
		return []string{remotescreen.DefaultSTUNURL}
	}
	return m.cfg.STUNURLs
}

func (m *ScreenManager) turn() *remotescreen.TURNOptions {
	if len(m.cfg.TURN.URLs) == 0 {
		return nil
	}
	return &remotescreen.TURNOptions{
		URLs: m.cfg.TURN.URLs, Username: m.cfg.TURN.Username, Credential: m.cfg.TURN.Credential,
	}
}

func (m *ScreenManager) iceServers() []ICEServer {
	servers := []ICEServer{{URLs: m.stunURLs()}}
	if turn := m.turn(); turn != nil {
		servers = append(servers, ICEServer{
			URLs: turn.URLs, Username: turn.Username, Credential: turn.Credential,
		})
	}
	return servers
}

// displayBounds reports the captured display's pixel rectangle. macOS uses a
// one-shot screencapture; other platforms use a conservative bound that the
// platform input adapter still clamps.
func displayBounds() image.Rectangle {
	if runtime.GOOS == "darwin" {
		file, err := os.CreateTemp("", "ukk-bounds-*.png")
		if err == nil {
			name := file.Name()
			file.Close()
			defer os.Remove(name)
			cmd := exec.Command("screencapture", "-x", "-t", "png", name)
			if err := cmd.Run(); err == nil {
				if handle, err := os.Open(name); err == nil {
					if cfg, err := png.DecodeConfig(handle); err == nil && cfg.Width > 0 && cfg.Height > 0 {
						handle.Close()
						return image.Rect(0, 0, cfg.Width, cfg.Height)
					}
					handle.Close()
				}
			}
		}
	}
	return image.Rect(0, 0, 4096, 4096)
}
