package bridge

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"sync"
	"time"

	"github.com/skip2/go-qrcode"
	"github.com/suj1e/ukk/apps/desktop/internal/remote"
)

const (
	pairingWindow   = 60 * time.Second
	defaultGrantTTL = 30 * 24 * time.Hour
	maxGrantMinutes = 60
)

// RemoteManager owns device identity, pairing, live grants and remote
// connections. It talks to the relay outbound and never accepts inbound WAN
// connections.
type RemoteManager struct {
	config   Config
	dir      string
	log      *slog.Logger
	server   *Server
	identity *remote.Identity
	invites  *remote.InviteStore
	pairer   *remote.HostPairer
	registry *remote.Registry
	relay    *relayHost

	mu         sync.Mutex
	lastInvite *Invite
	pending    *pendingPair
	sessions   map[string]*deviceSession
}

type pendingPair struct {
	code     string
	expireMS int64
	decision chan bool
}

type deviceSession struct {
	device   remote.Device
	conn     *remote.Conn
	sendMu   sync.Mutex
	closed   bool
	closeErr error
}

// NewRemoteManager loads or creates the host identity and trust registry.
func NewRemoteManager(config Config, dir string, server *Server, log *slog.Logger) (*RemoteManager, error) {
	if server == nil {
		return nil, errors.New("bridge: server required for remote events")
	}
	if log == nil {
		log = slog.Default()
	}
	identityPath := filepath.Join(dir, "identity.json")
	var identity *remote.Identity
	if _, err := os.Stat(identityPath); err == nil {
		identity, err = remote.LoadIdentity(identityPath)
		if err != nil {
			return nil, err
		}
	} else {
		generated, err := remote.GenerateIdentity()
		if err != nil {
			return nil, err
		}
		if err := remote.SaveIdentity(identityPath, generated); err != nil {
			return nil, err
		}
		identity = generated
	}
	manager := &RemoteManager{
		config:   config,
		dir:      dir,
		log:      log,
		server:   server,
		identity: identity,
		invites:  remote.NewInviteStore(),
		registry: remote.NewRegistry(),
		sessions: map[string]*deviceSession{},
	}
	if err := manager.loadTrust(); err != nil {
		return nil, err
	}
	pairer, err := remote.NewHostPairer(identity, config.HostID, manager.invites)
	if err != nil {
		return nil, err
	}
	pairer.Confirm = manager.confirmPairing
	manager.pairer = pairer
	return manager, nil
}

// Start connects to the relay when configured and forwards Agent events to
// authorized devices. It returns immediately.
func (m *RemoteManager) Start(ctx context.Context) {
	events, cancel := m.server.SubscribeEvents()
	go func() {
		defer cancel()
		for event := range events {
			m.forward(event)
		}
	}()
	if m.config.RelayURL == "" || m.config.OperatorToken == "" {
		m.log.Info("remote access idle: relay_url or operator_token missing")
		return
	}
	m.relay = newRelayHost(m.config.RelayURL, m.config.OperatorToken, m.config.HostID, m.log, m.acceptConn)
	go m.relay.Run(ctx)
}

// Status reports pairing and trust state.
func (m *RemoteManager) Status() Status {
	devices, grants := m.registry.Snapshot()
	out := Status{
		HostID:         m.config.HostID,
		RelayURL:       m.config.RelayURL,
		RelayConnected: m.relay != nil && m.relay.Connected(),
		Version:        Version,
	}
	scopes := map[string][]string{}
	expiry := map[string]int64{}
	for _, grant := range grants {
		scopes[grant.DeviceID] = append(scopes[grant.DeviceID], string(grant.Scope))
		if until := grant.ExpiresAt.UnixMilli(); until > expiry[grant.DeviceID] {
			expiry[grant.DeviceID] = until
		}
	}
	for _, device := range devices {
		out.Devices = append(out.Devices, Device{
			ID: device.ID, Label: "Paired device " + device.ID[len(device.ID)-6:],
			Scopes: scopes[device.ID], Revoked: device.Revoked,
			CreatedMS: device.CreatedAt.UnixMilli(), ExpiresMS: expiry[device.ID],
		})
	}
	m.mu.Lock()
	if m.lastInvite != nil && m.lastInvite.ExpireMS > time.Now().UnixMilli() {
		invite := *m.lastInvite
		out.Invite = &invite
	}
	if m.pending != nil {
		out.PairingCode = m.pending.code
		out.PairingExpireMS = m.pending.expireMS
	}
	m.mu.Unlock()
	return out
}

// Invite creates a one-time pairing offer and its QR image.
func (m *RemoteManager) Invite(ctx context.Context) (Invite, error) {
	if m.config.RelayURL == "" {
		return Invite{}, &Error{Code: "relay_required", Message: "configure relay_url in bridge.json before pairing a device"}
	}
	invite, err := remote.NewInvite(m.config.HostID, m.identity, m.config.RelayURL, 2*time.Minute)
	if err != nil {
		return Invite{}, err
	}
	payload, err := invite.Marshal()
	if err != nil {
		return Invite{}, err
	}
	if err := m.invites.Add(invite); err != nil {
		return Invite{}, err
	}
	png, err := qrcode.Encode(string(payload), qrcode.Medium, 512)
	if err != nil {
		return Invite{}, err
	}
	out := Invite{Payload: string(payload), QRPNG: png, ExpireMS: invite.ExpireMS, RelayURL: invite.Relay}
	m.mu.Lock()
	m.lastInvite = &out
	m.mu.Unlock()
	return out, nil
}

// Revoke removes a device and its live connections.
func (m *RemoteManager) Revoke(_ context.Context, deviceID string) error {
	if deviceID == "" {
		return &Error{Code: "invalid_params", Message: "device_id required"}
	}
	if err := m.registry.Revoke(deviceID, time.Now()); err != nil {
		return err
	}
	if err := m.persistTrust(); err != nil {
		return err
	}
	m.mu.Lock()
	session := m.sessions[deviceID]
	delete(m.sessions, deviceID)
	m.mu.Unlock()
	if session != nil {
		_ = session.conn.Close()
	}
	if m.relay != nil {
		m.relay.revoke(deviceID)
	}
	return nil
}

// Grant adds one explicit capability. Sensitive scopes are capped at an hour.
func (m *RemoteManager) Grant(_ context.Context, params GrantParams) error {
	scope := remote.Scope(params.Scope)
	if !scope.Valid() {
		return &Error{Code: "invalid_params", Message: "unknown scope"}
	}
	if scope == remote.ScopeDeviceManage {
		return &Error{Code: "not_grantable", Message: "device.manage is local only"}
	}
	minutes := params.Minutes
	if minutes <= 0 {
		minutes = 60
	}
	if scope.Sensitive() && minutes > maxGrantMinutes {
		return &Error{Code: "invalid_params", Message: "sensitive grants are capped at 60 minutes"}
	}
	expires := time.Now().Add(time.Duration(minutes) * time.Minute)
	grant := remote.Grant{
		DeviceID: params.DeviceID, Scope: scope,
		Subject: remote.SubjectHost, ExpiresAt: expires,
	}
	if err := m.registry.GrantFromPairing(grant, time.Now()); err != nil {
		return err
	}
	if err := m.persistTrust(); err != nil {
		return err
	}
	if m.relay != nil {
		m.relay.repair(params.DeviceID)
	}
	return nil
}

// Confirm answers a pending host-side pairing prompt.
func (m *RemoteManager) Confirm(_ context.Context, params ConfirmParams) error {
	m.mu.Lock()
	pending := m.pending
	m.mu.Unlock()
	if pending == nil {
		return &Error{Code: "no_pending_pairing", Message: "no pairing is waiting for confirmation"}
	}
	select {
	case pending.decision <- params.Approve:
	default:
	}
	return nil
}

func (m *RemoteManager) confirmPairing(ctx context.Context, code string) error {
	decision := make(chan bool, 1)
	pending := &pendingPair{code: code, expireMS: time.Now().Add(pairingWindow).UnixMilli(), decision: decision}
	m.mu.Lock()
	m.pending = pending
	m.mu.Unlock()
	m.server.Publish(Event{Type: "pairing", Error: code})
	defer func() {
		m.mu.Lock()
		if m.pending == pending {
			m.pending = nil
		}
		m.mu.Unlock()
	}()
	select {
	case approved := <-decision:
		if !approved {
			return errors.New("pairing denied by the host user")
		}
		return nil
	case <-time.After(pairingWindow):
		return errors.New("pairing confirmation timed out")
	case <-ctx.Done():
		return ctx.Err()
	}
}

// acceptConn handles one routed device connection: pairing or resume, then
// commands and events.
func (m *RemoteManager) acceptConn(connID string, transport *relayConn) {
	defer transport.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Minute)
	defer cancel()
	first, err := transport.Receive(ctx)
	if err != nil {
		return
	}
	var probe struct {
		T string `json:"t"`
	}
	if err := json.Unmarshal(first, &probe); err != nil {
		return
	}
	replay := &replayTransport{base: transport, first: first}

	var device remote.Device
	var session *remote.Session
	switch probe.T {
	case "c1":
		pairing, err := m.pairer.Accept(ctx, replay)
		if err != nil {
			m.log.Warn("pairing failed", "error", err)
			return
		}
		device = remote.Device{
			ID: pairing.DeviceID, Ed: pairing.Ed, X: pairing.X,
			Generation: 1, CreatedAt: time.Now(),
		}
		m.registry.Remove(device.ID)
		if err := m.registry.Register(device, time.Now()); err != nil {
			m.log.Warn("register paired device", "error", err)
			return
		}
		for _, scope := range []remote.Scope{remote.ScopeWorkspaceRead, remote.ScopeSessionObserve, remote.ScopeFilesystemRead, remote.ScopeAgentSend} {
			grant := remote.Grant{
				DeviceID: device.ID, Scope: scope, Subject: remote.SubjectHost,
				ExpiresAt: time.Now().Add(defaultGrantTTL),
			}
			if err := m.registry.GrantFromPairing(grant, time.Now()); err != nil {
				m.log.Warn("grant default scope", "error", err)
			}
		}
		if err := m.persistTrust(); err != nil {
			m.log.Error("persist trust", "error", err)
		}
		m.server.Publish(Event{Type: "pairing_complete"})
		session = pairing.Session
		if m.relay != nil {
			m.relay.repair(device.ID)
		}
	case "r1":
		resumed, deviceID, err := remote.HostResume(ctx, replay, m.config.HostID, m.identity, m.lookupDevice)
		if err != nil {
			m.log.Warn("resume failed", "error", err)
			return
		}
		record, ok := m.registry.Device(deviceID)
		if !ok || record.Revoked {
			return
		}
		device = record
		session = resumed
	default:
		return
	}

	conn, err := remote.NewConn(transport, session, remote.RoleHost)
	if err != nil {
		return
	}
	m.serve(device, conn, ctx)
}

func (m *RemoteManager) lookupDevice(ed remote.Key) (remote.DeviceRecord, bool) {
	device, ok := m.registry.LookupByEd(ed)
	if !ok || device.Revoked {
		return remote.DeviceRecord{}, false
	}
	generation := device.Generation
	if generation > uint64(^uint32(0)) {
		generation = uint64(^uint32(0))
	}
	return remote.DeviceRecord{
		ID: device.ID, Ed: device.Ed, X: device.X,
		Revoked: device.Revoked, Generation: uint32(generation),
	}, true
}

func (m *RemoteManager) serve(device remote.Device, conn *remote.Conn, ctx context.Context) {
	session := &deviceSession{device: device, conn: conn}
	m.mu.Lock()
	if previous := m.sessions[device.ID]; previous != nil {
		_ = previous.conn.Close()
	}
	m.sessions[device.ID] = session
	m.mu.Unlock()
	defer func() {
		m.mu.Lock()
		if m.sessions[device.ID] == session {
			delete(m.sessions, device.ID)
		}
		m.mu.Unlock()
		_ = conn.Close()
	}()

	authz := &deviceAuthorizer{manager: m, device: device}
	for {
		stream, plaintext, err := conn.ReceiveEnvelope(ctx)
		if err != nil {
			return
		}
		if stream != 0 {
			continue
		}
		var request Request
		if err := json.Unmarshal(plaintext, &request); err != nil || request.ID == "" || request.Method == "" {
			continue
		}
		result, dispatchErr := m.server.Dispatch(ctx, request.Method, request.Params, authz)
		response := Response{ID: request.ID}
		if dispatchErr != nil {
			response.Error = toWireError(dispatchErr)
		} else if result != nil {
			encoded, err := json.Marshal(result)
			if err == nil {
				response.Result = encoded
			} else {
				response.Error = &Error{Code: "internal", Message: "encode failed"}
			}
		}
		raw, err := json.Marshal(response)
		if err != nil {
			continue
		}
		if err := conn.SendEnvelope(0, raw); err != nil {
			return
		}
	}
}

func (m *RemoteManager) forward(event Event) {
	if event.Agent == nil || event.SessionID == "" {
		return
	}
	m.mu.Lock()
	sessions := make([]*deviceSession, 0, len(m.sessions))
	for _, session := range m.sessions {
		sessions = append(sessions, session)
	}
	m.mu.Unlock()
	if len(sessions) == 0 {
		return
	}
	payload, err := json.Marshal(map[string]any{"type": "event", "event": event})
	if err != nil {
		return
	}
	for _, session := range sessions {
		err := m.registry.Authorize(session.device.ID, remote.ScopeSessionObserve,
			remote.SubjectSession(event.SessionID), session.device.Generation, time.Now())
		if err != nil {
			continue
		}
		session.sendMu.Lock()
		sendErr := session.conn.SendEnvelope(0, payload)
		session.sendMu.Unlock()
		if sendErr != nil {
			_ = session.conn.Close()
		}
	}
}

type replayTransport struct {
	base  *relayConn
	first []byte
}

func (r *replayTransport) Send(data []byte) error { return r.base.Send(data) }

func (r *replayTransport) Receive(ctx context.Context) ([]byte, error) {
	if r.first != nil {
		data := r.first
		r.first = nil
		return data, nil
	}
	return r.base.Receive(ctx)
}

type deviceAuthorizer struct {
	manager *RemoteManager
	device  remote.Device
}

func (a *deviceAuthorizer) Allow(_ context.Context, method string, params json.RawMessage) error {
	if method == MethodHealth {
		return nil
	}
	scope, ok := scopeForMethod(method)
	if !ok {
		return &Denied{Reason: "method is not available to paired devices"}
	}
	var probe struct {
		SessionID string `json:"session_id"`
	}
	_ = json.Unmarshal(params, &probe)
	subject := remote.SubjectHost
	if probe.SessionID != "" {
		subject = remote.SubjectSession(probe.SessionID)
	}
	grantScope := scope
	if method == MethodAgentSnapshot && probe.SessionID == "" {
		grantScope = remote.ScopeWorkspaceRead
	}
	if err := a.manager.registry.Authorize(a.device.ID, grantScope, subject, a.device.Generation, time.Now()); err != nil {
		return &Denied{Reason: err.Error()}
	}
	return nil
}

func scopeForMethod(method string) (remote.Scope, bool) {
	switch method {
	case MethodWorkspaceGet:
		return remote.ScopeWorkspaceRead, true
	case MethodAgentList:
		return remote.ScopeSessionObserve, true
	case MethodAgentSnapshot:
		return remote.ScopeSessionObserve, true
	case MethodAgentPrompt, MethodAgentAbort, MethodAgentStop:
		return remote.ScopeAgentSend, true
	case MethodAgentApprove:
		return remote.ScopeAgentApprove, true
	case MethodRepoStatus, MethodRepoDiff, MethodRepoFile:
		return remote.ScopeFilesystemRead, true
	case MethodScreenStart, MethodScreenAnswer, MethodScreenStop:
		return remote.ScopeScreenView, true
	case MethodScreenInput:
		return remote.ScopeScreenControl, true
	case MethodPluginList, MethodPluginInvoke, MethodPluginEnable, MethodPluginDisable,
		MethodPluginGrant, MethodPluginRevoke, MethodPluginInstall:
		return remote.ScopePluginControl, true
	default:
		return "", false
	}
}

type trustDevice struct {
	ID         string     `json:"id"`
	Ed         remote.Key `json:"ed"`
	X          remote.Key `json:"x"`
	Generation uint64     `json:"generation"`
	Revoked    bool       `json:"revoked"`
	CreatedMS  int64      `json:"created_ms"`
}

type trustGrant struct {
	DeviceID  string `json:"device_id"`
	Scope     string `json:"scope"`
	Subject   string `json:"subject"`
	ExpiresMS int64  `json:"expires_ms"`
}

type trustFile struct {
	V       int           `json:"v"`
	Devices []trustDevice `json:"devices"`
	Grants  []trustGrant  `json:"grants"`
}

func (m *RemoteManager) loadTrust() error {
	path := filepath.Join(m.dir, "trust.json")
	raw, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	var file trustFile
	if err := json.Unmarshal(raw, &file); err != nil {
		return fmt.Errorf("bridge: decode trust: %w", err)
	}
	if file.V != 1 {
		return errors.New("bridge: unsupported trust revision")
	}
	devices := make([]remote.Device, 0, len(file.Devices))
	for _, device := range file.Devices {
		devices = append(devices, remote.Device{
			ID: device.ID, Ed: device.Ed, X: device.X,
			Generation: device.Generation, Revoked: device.Revoked,
			CreatedAt: time.UnixMilli(device.CreatedMS),
		})
	}
	grants := make([]remote.Grant, 0, len(file.Grants))
	for _, grant := range file.Grants {
		grants = append(grants, remote.Grant{
			DeviceID: grant.DeviceID, Scope: remote.Scope(grant.Scope),
			Subject: grant.Subject, ExpiresAt: time.UnixMilli(grant.ExpiresMS),
		})
	}
	return m.registry.Restore(devices, grants, time.Now())
}

func (m *RemoteManager) persistTrust() error {
	devices, grants := m.registry.Snapshot()
	file := trustFile{V: 1}
	for _, device := range devices {
		file.Devices = append(file.Devices, trustDevice{
			ID: device.ID, Ed: device.Ed, X: device.X,
			Generation: device.Generation, Revoked: device.Revoked,
			CreatedMS: device.CreatedAt.UnixMilli(),
		})
	}
	for _, grant := range grants {
		file.Grants = append(file.Grants, trustGrant{
			DeviceID: grant.DeviceID, Scope: string(grant.Scope),
			Subject: grant.Subject, ExpiresMS: grant.ExpiresAt.UnixMilli(),
		})
	}
	raw, err := json.MarshalIndent(file, "", "  ")
	if err != nil {
		return err
	}
	return writeAtomic(filepath.Join(m.dir, "trust.json"), append(raw, '\n'))
}
