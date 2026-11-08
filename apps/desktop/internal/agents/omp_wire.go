package agents

import (
	"encoding/json"
	"fmt"
	"strings"
)

// Wire facts verified against omp 18.18.1 (`omp --mode rpc`), its pinned RPC
// schema (packages/coding-agent/src/modes/rpc/wire), and the previously
// verified client facts at git f033cf0 crates/omp-client/src/wire.rs.
const (
	// ompRPCPROTOCOLVersion is the only protocol version this adapter speaks.
	ompRPCProtocolVersion = 1

	// Frame caps. The child advertises maxFrameBytes in `ready`; the adapter
	// clamps it into [minFrameBytes, absoluteMaxFrameBytes].
	minFrameBytes          = 1024
	defaultMaxFrameBytes   = 1 << 20
	absoluteMaxFrameBytes  = 8 << 20
	frameDrainFactor       = 2
	maxStderrLineBytes     = 4096
	maxStderrDrainBytes    = 256 << 10
	maxDiagnosticBytes     = 2048
	maxPromptBytes         = 512 << 10
	ompRPCReadyFrameBudget = 4096
)

// readyInfo is the validated handshake advertised by the child.
type readyInfo struct {
	protocolVersion       int
	supportedVersions     []int
	maxFrameBytes         int
	maxReassembledBytes   int
	supportsBashExclusion bool
}

// readyFrame mirrors the documented `ready` frame. Transport fields are
// optional in the schema but required by this client for a v1 line transport.
type readyFrame struct {
	Type                           string `json:"type"`
	ProtocolVersion                *int   `json:"protocolVersion"`
	SupportedProtocolVersions      []int  `json:"supportedProtocolVersions"`
	MaxFrameBytes                  *int64 `json:"maxFrameBytes"`
	MaxReassembledFrameBytes       int64  `json:"maxReassembledFrameBytes"`
	SupportsBashExcludeFromContext bool   `json:"supportsBashExcludeFromContext"`
}

// parseReady validates the first stdout frame. Any mismatch is a handshake
// failure and the caller kills the child.
func parseReady(line []byte) (readyInfo, error) {
	if len(line) > ompRPCReadyFrameBudget {
		return readyInfo{}, NewError(CodeProtocol, "omp ready frame exceeded %d bytes", ompRPCReadyFrameBudget)
	}
	var frame readyFrame
	if err := json.Unmarshal(line, &frame); err != nil {
		return readyInfo{}, NewError(CodeProtocol, "first omp frame is not valid JSON: %s", boundedMessage(err.Error(), 256))
	}
	if frame.Type != "ready" {
		return readyInfo{}, NewError(CodeProtocol, "expected an omp ready frame, got %q", frame.Type)
	}
	if frame.ProtocolVersion == nil {
		return readyInfo{}, NewError(CodeProtocol, "omp ready frame is missing protocolVersion")
	}
	if *frame.ProtocolVersion != ompRPCProtocolVersion {
		return readyInfo{}, NewError(CodeProtocol, "unsupported omp protocol version %d; this adapter speaks v%d", *frame.ProtocolVersion, ompRPCProtocolVersion)
	}
	if len(frame.SupportedProtocolVersions) > 0 {
		found := false
		for _, version := range frame.SupportedProtocolVersions {
			if version == ompRPCProtocolVersion {
				found = true
				break
			}
		}
		if !found {
			return readyInfo{}, NewError(CodeProtocol, "omp advertises no supported protocol v%d", ompRPCProtocolVersion)
		}
	}
	if frame.MaxFrameBytes == nil {
		return readyInfo{}, NewError(CodeProtocol, "omp ready frame is missing maxFrameBytes")
	}
	advertised := *frame.MaxFrameBytes
	if advertised < minFrameBytes {
		return readyInfo{}, NewError(CodeProtocol, "omp advertised an unusable maxFrameBytes (%d)", advertised)
	}
	if advertised > absoluteMaxFrameBytes {
		advertised = absoluteMaxFrameBytes
	}
	return readyInfo{
		protocolVersion:       *frame.ProtocolVersion,
		supportedVersions:     frame.SupportedProtocolVersions,
		maxFrameBytes:         int(advertised),
		maxReassembledBytes:   int(frame.MaxReassembledFrameBytes),
		supportsBashExclusion: frame.SupportsBashExcludeFromContext,
	}, nil
}

// responseFrame is one correlated command response.
type responseFrame struct {
	Type    string          `json:"type"`
	ID      string          `json:"id"`
	Command string          `json:"command"`
	Success *bool           `json:"success"`
	Error   string          `json:"error"`
	Code    string          `json:"code"`
	Data    json.RawMessage `json:"data"`
}

// rpcEvent is the union of the documented event frames this adapter reads. A
// frame only carries the fields its `type` documents.
type rpcEvent struct {
	Type string `json:"type"`

	// Message lifecycle.
	MessageID             string          `json:"messageId"`
	Message               json.RawMessage `json:"message"`
	AssistantMessageEvent *assistantEvent `json:"assistantMessageEvent"`

	// Tool lifecycle.
	ToolCallID    string          `json:"toolCallId"`
	ToolName      string          `json:"toolName"`
	Args          json.RawMessage `json:"args"`
	PartialResult json.RawMessage `json:"partialResult"`
	Result        json.RawMessage `json:"result"`
	IsError       *bool           `json:"isError"`

	// Run lifecycle.
	IsTerminal   *bool           `json:"isTerminal"`
	AgentInvoked *bool           `json:"agentInvoked"`
	Status       string          `json:"status"`
	Error        json.RawMessage `json:"error"`

	// Extension failures.
	ExtensionPath string `json:"extensionPath"`
	Event         string `json:"event"`

	// Session information.
	SessionID string `json:"sessionId"`

	// Command output.
	Text string `json:"text"`
}

// uiRequestFrame is the documented ExtensionUiRequest subset used by approvals.
type uiRequestFrame struct {
	Type      string   `json:"type"`
	ID        string   `json:"id"`
	Method    string   `json:"method"`
	Title     string   `json:"title"`
	Message   string   `json:"message"`
	Options   []string `json:"options"`
	TimeoutMS int64    `json:"timeout"`
	TargetID  string   `json:"targetId"`
}

// noticeFrame is the documented NoticeEvent.
type noticeFrame struct {
	Type    string `json:"type"`
	Level   string `json:"level"`
	Message string `json:"message"`
	Source  string `json:"source"`
}

// promptError is the documented failure detail of a prompt_result.
type promptError struct {
	Message    string `json:"message"`
	Provider   string `json:"provider"`
	Model      string `json:"model"`
	HTTPStatus int    `json:"httpStatus"`
	Retryable  bool   `json:"retryable"`
	Code       string `json:"code"`
}

// assistantEvent is the streaming update of one assistant message.
type assistantEvent struct {
	Type     string          `json:"type"`
	Delta    string          `json:"delta"`
	Content  string          `json:"content"`
	Message  json.RawMessage `json:"message"`
	Error    json.RawMessage `json:"error"`
	ToolCall json.RawMessage `json:"toolCall"`
}

// agentMessage is the documented AgentMessage subset needed for the contract.
type agentMessage struct {
	Role         string          `json:"role"`
	Content      json.RawMessage `json:"content"`
	ToolCallID   string          `json:"toolCallId"`
	ToolName     string          `json:"toolName"`
	IsError      bool            `json:"isError"`
	StopReason   string          `json:"stopReason"`
	ErrorMessage string          `json:"errorMessage"`
	Timestamp    int64           `json:"timestamp"`
	Usage        *usageFrame     `json:"usage"`
}

// usageFrame is the documented provider Usage record.
type usageFrame struct {
	Input           int64 `json:"input"`
	Output          int64 `json:"output"`
	CacheRead       int64 `json:"cacheRead"`
	CacheWrite      int64 `json:"cacheWrite"`
	TotalTokens     int64 `json:"totalTokens"`
	ContextTokens   int64 `json:"contextTokens"`
	ReasoningTokens int64 `json:"reasoningTokens"`
	Cost            struct {
		Total float64 `json:"total"`
	} `json:"cost"`
}

// contentBlock is one block of message content.
type contentBlock struct {
	Type string `json:"type"`
	Text string `json:"text"`
}

// messageText joins the text blocks of a message. Thinking and tool-call blocks
// are deliberately excluded from visible answer text.
func messageText(raw json.RawMessage) string {
	if len(raw) == 0 {
		return ""
	}
	var plain string
	if err := json.Unmarshal(raw, &plain); err == nil {
		return plain
	}
	var blocks []contentBlock
	if err := json.Unmarshal(raw, &blocks); err != nil {
		return ""
	}
	var out strings.Builder
	for _, block := range blocks {
		if block.Type == "text" {
			out.WriteString(block.Text)
		}
	}
	return out.String()
}

// decodeAgentMessage parses one AgentMessage frame.
func decodeAgentMessage(raw json.RawMessage) (agentMessage, bool) {
	if len(raw) == 0 {
		return agentMessage{}, false
	}
	var message agentMessage
	if err := json.Unmarshal(raw, &message); err != nil {
		return agentMessage{}, false
	}
	return message, true
}

// usageFromFrame converts provider usage into the contract type. Nil means the
// provider did not report usage.
func usageFromFrame(frame *usageFrame) (Usage, bool) {
	if frame == nil {
		return Usage{}, false
	}
	return Usage{
		Reported:         true,
		InputTokens:      frame.Input,
		OutputTokens:     frame.Output,
		CacheReadTokens:  frame.CacheRead,
		CacheWriteTokens: frame.CacheWrite,
		TotalTokens:      frame.TotalTokens,
		ContextTokens:    frame.ContextTokens,
		ReasoningTokens:  frame.ReasoningTokens,
		CostUSD:          frame.Cost.Total,
	}, true
}

// commandFrame is one outbound command. Only the fields a command documents are
// populated.
type commandFrame struct {
	Type    string `json:"type"`
	ID      string `json:"id,omitempty"`
	Message string `json:"message,omitempty"`
}

// uiResponseFrame answers an extension UI request. Exactly one payload key is
// set, matching the request method.
type uiResponseFrame struct {
	Type      string  `json:"type"`
	ID        string  `json:"id"`
	Value     *string `json:"value,omitempty"`
	Confirmed *bool   `json:"confirmed,omitempty"`
	Cancelled *bool   `json:"cancelled,omitempty"`
}

// approvalTitlePrefix is the documented omp approval dialog title prefix. The
// RPC v1 transport carries tool approvals as `select` UI requests; the tool
// name is the only structured fact in the title.
const approvalTitlePrefix = "Allow tool: "

// approvalFromUI recognizes a tool approval request. omp renders approvals as
// an extension UI `select` whose options include an approve choice and "Deny"
// (jre/Uqe in omp 18.18.1), or as a `confirm` whose title names the tool.
func approvalFromUI(frame *uiRequestFrame, nowMS int64) (Approval, bool) {
	if frame == nil || frame.ID == "" {
		return Approval{}, false
	}
	title := strings.TrimSpace(frame.Title)
	if title == "" {
		title = strings.TrimSpace(frame.Message)
	}
	toolName := ""
	if strings.HasPrefix(title, approvalTitlePrefix) {
		firstLine := title
		if index := strings.IndexByte(firstLine, '\n'); index >= 0 {
			firstLine = firstLine[:index]
		}
		toolName = strings.TrimSpace(strings.TrimPrefix(firstLine, approvalTitlePrefix))
	}
	switch frame.Method {
	case "select":
		if toolName == "" && !(hasOption(frame.Options, "Deny") && hasApproveOption(frame.Options)) {
			return Approval{}, false
		}
	case "confirm":
		if toolName == "" {
			return Approval{}, false
		}
	default:
		return Approval{}, false
	}
	approval := Approval{
		ID:       frame.ID,
		ToolName: toolName,
		State:    ApprovalPending,
	}
	if frame.TimeoutMS > 0 {
		approval.ExpiresMS = nowMS + frame.TimeoutMS
	}
	return approval, true
}

func hasOption(options []string, want string) bool {
	for _, option := range options {
		if strings.EqualFold(strings.TrimSpace(option), want) {
			return true
		}
	}
	return false
}

func hasApproveOption(options []string) bool {
	for _, option := range options {
		trimmed := strings.TrimSpace(option)
		if strings.HasPrefix(strings.ToLower(trimmed), "approve") && !strings.HasPrefix(strings.ToLower(trimmed), "always") {
			return true
		}
	}
	return false
}

// approvalOption picks the option label that answers the approval without
// granting a broader permission than the caller asked for. omp labels the plain
// choice "Approve" or "Approve Once"; broader grants ("Approve File", "Always
// Approve") are never selected implicitly.
func approvalOption(options []string, decision Decision) (string, bool) {
	switch decision {
	case DecisionApprove:
		for _, want := range []string{"Approve Once", "Approve"} {
			for _, option := range options {
				if strings.EqualFold(strings.TrimSpace(option), want) {
					return option, true
				}
			}
		}
		for _, option := range options {
			trimmed := strings.TrimSpace(option)
			lower := strings.ToLower(trimmed)
			if strings.HasPrefix(lower, "approve") && !strings.HasPrefix(lower, "always") {
				return option, true
			}
		}
		return "", false
	case DecisionDeny:
		for _, option := range options {
			if strings.EqualFold(strings.TrimSpace(option), "Deny") {
				return option, true
			}
		}
		return "", false
	default:
		return "", false
	}
}

// approvalAnswer builds the UI response that settles an approval.
func approvalAnswer(method string, options []string, decision Decision) (uiResponseFrame, error) {
	if decision == DecisionCancel {
		return uiResponseFrame{Type: "extension_ui_response", Cancelled: new(true)}, nil
	}
	switch method {
	case "select":
		option, ok := approvalOption(options, decision)
		if !ok {
			return uiResponseFrame{}, NewError(CodeProtocol, "omp approval offered no option for decision %q", decision)
		}
		return uiResponseFrame{Type: "extension_ui_response", Value: &option}, nil
	case "confirm":
		confirmed := decision == DecisionApprove
		return uiResponseFrame{Type: "extension_ui_response", Confirmed: new(confirmed)}, nil
	default:
		return uiResponseFrame{}, NewError(CodeUnsupported, "omp approval method %q cannot be answered", method)
	}
}

// sessionStateFrame is the documented SessionState subset used at startup.
type sessionStateFrame struct {
	SessionID string `json:"sessionId"`
}

func decodeSessionState(raw json.RawMessage) (sessionStateFrame, error) {
	var state sessionStateFrame
	if len(raw) == 0 {
		return state, fmt.Errorf("missing data")
	}
	if err := json.Unmarshal(raw, &state); err != nil {
		return state, err
	}
	return state, nil
}

// rawText decodes a JSON string payload, falling back to the raw bytes for
// shapes that are not plain strings.
func rawText(raw json.RawMessage) string {
	if len(raw) == 0 {
		return ""
	}
	var text string
	if err := json.Unmarshal(raw, &text); err == nil {
		return text
	}
	return string(raw)
}
