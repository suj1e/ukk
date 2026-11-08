package bridge

import (
	"bytes"
	"encoding/json"
)

// Wire methods. The same names serve the local HTTP API and paired remote
// devices, so one dispatcher owns authorization and behavior.
const (
	MethodHealth        = "health"
	MethodWorkspaceGet  = "workspace.get"
	MethodWorkspaceSet  = "workspace.set"
	MethodAgentStart    = "agent.start"
	MethodAgentSnapshot = "agent.snapshot"
	MethodAgentList     = "agent.list"
	MethodAgentPrompt   = "agent.prompt"
	MethodAgentAbort    = "agent.abort"
	MethodAgentStop     = "agent.stop"
	MethodAgentApprove  = "agent.approve"
	MethodRepoStatus    = "repo.status"
	MethodRepoDiff      = "repo.diff"
	MethodRepoFile      = "repo.file"
	MethodRemoteStatus  = "remote.status"
	MethodRemoteInvite  = "remote.invite"
	MethodRemoteRevoke  = "remote.revoke"
	MethodRemoteGrant   = "remote.grant"
	MethodRemoteConfirm = "remote.confirm"
	MethodScreenStart   = "screen.start"
	MethodScreenAnswer  = "screen.answer"
	MethodScreenStop    = "screen.stop"
	MethodScreenInput   = "screen.input"
	MethodPluginList    = "plugin.list"
	MethodPluginInstall = "plugin.install"
	MethodPluginEnable  = "plugin.enable"
	MethodPluginDisable = "plugin.disable"
	MethodPluginGrant   = "plugin.grant"
	MethodPluginRevoke  = "plugin.revoke"
	MethodPluginInvoke  = "plugin.invoke"
)

// Error is a typed command failure.
type Error struct {
	Code    string `json:"code"`
	Message string `json:"message"`
}

func (e *Error) Error() string { return e.Code + ": " + e.Message }

// Request is one command.
type Request struct {
	ID     string          `json:"id"`
	Method string          `json:"method"`
	Params json.RawMessage `json:"params,omitempty"`
}

// Response answers a Request.
type Response struct {
	ID     string          `json:"id"`
	Result json.RawMessage `json:"result,omitempty"`
	Error  *Error          `json:"error,omitempty"`
}

// Message is one Agent message as shown by every client.
type Message struct {
	ID        string `json:"id"`
	Role      string `json:"role"`
	Text      string `json:"text"`
	Final     bool   `json:"final"`
	CreatedMS int64  `json:"created_ms"`
}

// Tool is one provider-reported tool call.
type Tool struct {
	ID        string          `json:"id"`
	TurnID    string          `json:"turn_id,omitempty"`
	Name      string          `json:"name"`
	Input     json.RawMessage `json:"input,omitempty"`
	Output    json.RawMessage `json:"output,omitempty"`
	Status    string          `json:"status"`
	IsError   bool            `json:"is_error,omitempty"`
	StartedMS int64           `json:"started_ms,omitempty"`
	EndedMS   int64           `json:"ended_ms,omitempty"`
}

// Approval is a provider-emitted approval request.
type Approval struct {
	ID        string          `json:"id"`
	TurnID    string          `json:"turn_id,omitempty"`
	ToolName  string          `json:"tool_name"`
	Input     json.RawMessage `json:"input,omitempty"`
	State     string          `json:"state"`
	ExpiresMS int64           `json:"expires_ms,omitempty"`
}

// Turn is one Agent turn.
type Turn struct {
	ID        string `json:"id"`
	State     string `json:"state"`
	StartedMS int64  `json:"started_ms,omitempty"`
	EndedMS   int64  `json:"ended_ms,omitempty"`
	Error     string `json:"error,omitempty"`
}

// Usage is provider-reported usage.
type Usage struct {
	PromptTokens     int64 `json:"prompt_tokens,omitempty"`
	CompletionTokens int64 `json:"completion_tokens,omitempty"`
	TotalTokens      int64 `json:"total_tokens,omitempty"`
}

// AgentSnapshot is the complete bounded view of one Agent session.
type AgentSnapshot struct {
	Provider          string     `json:"provider"`
	ProviderSessionID string     `json:"provider_session_id,omitempty"`
	State             string     `json:"state"`
	Turns             []Turn     `json:"turns,omitempty"`
	Messages          []Message  `json:"messages,omitempty"`
	Tools             []Tool     `json:"tools,omitempty"`
	PendingApprovals  []Approval `json:"pending_approvals,omitempty"`
	Usage             Usage      `json:"usage,omitempty"`
	Error             string     `json:"error,omitempty"`
	Provenance        string     `json:"provenance,omitempty"`
	UpdatedMS         int64      `json:"updated_ms,omitempty"`
}

// StartParams begins a provider session.
type StartParams struct {
	SessionID string `json:"session_id"`
	Cwd       string `json:"cwd"`
	Adapter   string `json:"adapter,omitempty"`
	Model     string `json:"model,omitempty"`
}

// SessionParams names one Agent session.
type SessionParams struct {
	SessionID string `json:"session_id"`
}

// PromptParams sends user input.
type PromptParams struct {
	SessionID string `json:"session_id"`
	Text      string `json:"text"`
}

// ApproveParams answers a provider approval.
type ApproveParams struct {
	SessionID  string `json:"session_id"`
	ApprovalID string `json:"approval_id"`
	Decision   string `json:"decision"`
	Note       string `json:"note,omitempty"`
}

// WorkspaceParams mirrors the Desktop's durable metadata for remote clients.
type WorkspaceParams struct {
	Snapshot json.RawMessage `json:"snapshot"`
}

// Status is the remote subsystem's state.
type Status struct {
	HostID          string   `json:"host_id"`
	RelayConnected  bool     `json:"relay_connected"`
	RelayURL        string   `json:"relay_url,omitempty"`
	Devices         []Device `json:"devices"`
	Invite          *Invite  `json:"invite,omitempty"`
	PairingCode     string   `json:"pairing_code,omitempty"`
	PairingExpireMS int64    `json:"pairing_expire_ms,omitempty"`
	Version         int      `json:"version"`
}

// Device is one paired device.
type Device struct {
	ID        string   `json:"id"`
	Label     string   `json:"label"`
	Scopes    []string `json:"scopes"`
	ExpiresMS int64    `json:"expires_ms,omitempty"`
	Revoked   bool     `json:"revoked,omitempty"`
	CreatedMS int64    `json:"created_ms"`
}

// Invite is a short-lived pairing offer. The secret is part of the QR payload
// and is never returned separately to a second client.
type Invite struct {
	Payload  string `json:"payload"`
	QRPNG    []byte `json:"qr_png,omitempty"`
	ExpireMS int64  `json:"expire_ms"`
	RelayURL string `json:"relay_url,omitempty"`
}

// ConfirmParams answers a pending host-side pairing confirmation.
type ConfirmParams struct {
	Approve bool `json:"approve"`
}

// RevokeParams removes a device's access.
type RevokeParams struct {
	DeviceID string `json:"device_id"`
}

// GrantParams adds one explicit device capability. Sensitive scopes are
// bounded to 60 minutes.
type GrantParams struct {
	DeviceID string `json:"device_id"`
	Scope    string `json:"scope"`
	Minutes  int    `json:"minutes"`
}

// RepoParams selects repository data for a remote client.
type RepoParams struct {
	SessionID string `json:"session_id"`
	Path      string `json:"path,omitempty"`
	Staged    bool   `json:"staged,omitempty"`
}

// RepoFileResult is one file's bounded contents.
type RepoFileResult struct {
	Path    string `json:"path"`
	Content string `json:"content"`
	Binary  bool   `json:"binary"`
}

// ScreenStartParams opens an on-demand screen session.
type ScreenStartParams struct {
	SessionID string `json:"session_id"`
	View      bool   `json:"view"`
	Control   bool   `json:"control"`
}

// ScreenSession is the Host's WebRTC offer and ICE configuration.
type ScreenSession struct {
	SessionID  string      `json:"session_id"`
	SDP        string      `json:"sdp"`
	ICEServers []ICEServer `json:"ice_servers"`
	View       bool        `json:"view"`
	Control    bool        `json:"control"`
}

// ICEServer is one STUN/TURN endpoint for the media peer.
type ICEServer struct {
	URLs       []string `json:"urls"`
	Username   string   `json:"username,omitempty"`
	Credential string   `json:"credential,omitempty"`
}

// ScreenAnswerParams completes the media negotiation.
type ScreenAnswerParams struct {
	SessionID string `json:"session_id"`
	SDP       string `json:"sdp"`
}

// ScreenInputParams injects input, allowed only with a control grant.
type ScreenInputParams struct {
	SessionID string       `json:"session_id"`
	Events    []InputEvent `json:"events"`
}

// InputEvent is one pointer, wheel or key action in captured-display space.
type InputEvent struct {
	Kind   string  `json:"kind"`
	X      float64 `json:"x,omitempty"`
	Y      float64 `json:"y,omitempty"`
	Button int     `json:"button,omitempty"`
	DeltaX float64 `json:"delta_x,omitempty"`
	DeltaY float64 `json:"delta_y,omitempty"`
	Code   string  `json:"code,omitempty"`
	Down   bool    `json:"down,omitempty"`
}

// PluginInstallParams names a plugin directory to validate and register.
type PluginInstallParams struct {
	Dir string `json:"dir"`
}

// PluginParams names one plugin.
type PluginParams struct {
	Plugin string `json:"plugin"`
}

// PluginGrantParams adds one capability grant.
type PluginGrantParams struct {
	Plugin     string `json:"plugin"`
	Capability string `json:"capability"`
	Scope      string `json:"scope"`
}

// PluginInvokeParams runs one declared command.
type PluginInvokeParams struct {
	Plugin    string          `json:"plugin"`
	Command   string          `json:"command"`
	SessionID string          `json:"session_id,omitempty"`
	Args      json.RawMessage `json:"args,omitempty"`
}

// Event is one pushed change.
type Event struct {
	Type      string         `json:"type"`
	SessionID string         `json:"session_id,omitempty"`
	Agent     *AgentSnapshot `json:"agent,omitempty"`
	Error     string         `json:"error,omitempty"`
	AtMS      int64          `json:"at_ms"`
}

// Event types.
const (
	EventSnapshot = "snapshot"
	EventUpdate   = "update"
	EventError    = "error"
	EventScreen   = "screen"
)

// DecodeParams decodes command parameters with unknown fields rejected.
func DecodeParams(raw json.RawMessage, target any) error {
	if len(raw) == 0 {
		raw = []byte("{}")
	}
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(target); err != nil {
		return &Error{Code: "invalid_params", Message: err.Error()}
	}
	return nil
}
