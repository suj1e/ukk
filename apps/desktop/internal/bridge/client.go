package bridge

import (
	"bufio"
	"bytes"
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"github.com/suj1e/ukk/apps/desktop/internal/extensions"
)

// Client is a typed local bridge client.
type Client struct {
	http  *http.Client
	base  string
	token string
}

// NewClient dials the published local endpoint.
func NewClient(dir string) (*Client, error) {
	httpClient, endpoint, err := LocalHTTP(dir)
	if err != nil {
		return nil, err
	}
	raw, err := os.ReadFile(endpoint.Token)
	if err != nil {
		return nil, fmt.Errorf("bridge: read token: %w", err)
	}
	token := strings.TrimSpace(string(raw))
	if token == "" {
		return nil, errors.New("bridge: empty local token")
	}
	return &Client{http: httpClient, base: LocalBaseURL(endpoint), token: token}, nil
}

// Ensure starts the bridge when it is not already reachable, then returns a
// client. The bridge runs detached so closing the Desktop does not stop it.
func Ensure(ctx context.Context, dir string) (*Client, error) {
	if client, err := NewClient(dir); err == nil {
		healthCtx, cancel := context.WithTimeout(ctx, 2*time.Second)
		_, healthErr := client.Call(healthCtx, MethodHealth, nil)
		cancel()
		if healthErr == nil {
			return client, nil
		}
	}
	if err := StartDetached(dir); err != nil {
		return nil, err
	}
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		default:
		}
		if client, err := NewClient(dir); err == nil {
			healthCtx, cancel := context.WithTimeout(ctx, 2*time.Second)
			_, healthErr := client.Call(healthCtx, MethodHealth, nil)
			cancel()
			if healthErr == nil {
				return client, nil
			}
		}
		time.Sleep(150 * time.Millisecond)
	}
	return nil, errors.New("bridge: did not become ready; check the bridge log")
}

// Call runs one command.
func (c *Client) Call(ctx context.Context, method string, params any) (json.RawMessage, error) {
	var raw json.RawMessage
	if params != nil {
		encoded, err := json.Marshal(params)
		if err != nil {
			return nil, err
		}
		raw = encoded
	}
	request := Request{ID: newRequestID(), Method: method, Params: raw}
	body, err := json.Marshal(request)
	if err != nil {
		return nil, err
	}
	httpRequest, err := http.NewRequestWithContext(ctx, http.MethodPost, c.base+"/v1/command", bytes.NewReader(body))
	if err != nil {
		return nil, err
	}
	httpRequest.Header.Set("Authorization", "Bearer "+c.token)
	httpRequest.Header.Set("Content-Type", "application/json")
	response, err := c.http.Do(httpRequest)
	if err != nil {
		return nil, err
	}
	defer response.Body.Close()
	limited := io.LimitReader(response.Body, 8<<20)
	payload, err := io.ReadAll(limited)
	if err != nil {
		return nil, err
	}
	var decoded Response
	if err := json.Unmarshal(payload, &decoded); err != nil {
		return nil, fmt.Errorf("bridge: decode response (%s): %w", response.Status, err)
	}
	if decoded.Error != nil {
		return nil, decoded.Error
	}
	if response.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("bridge: unexpected status %s", response.Status)
	}
	return decoded.Result, nil
}

// CallInto runs a command and decodes its result.
func (c *Client) CallInto(ctx context.Context, method string, params, target any) error {
	result, err := c.Call(ctx, method, params)
	if err != nil {
		return err
	}
	if target == nil || len(result) == 0 {
		return nil
	}
	return json.Unmarshal(result, target)
}

// Health checks the bridge.
func (c *Client) Health(ctx context.Context) error {
	_, err := c.Call(ctx, MethodHealth, nil)
	return err
}

// StartAgent starts or returns a provider session.
func (c *Client) StartAgent(ctx context.Context, params StartParams) (AgentSnapshot, error) {
	var snapshot AgentSnapshot
	err := c.CallInto(ctx, MethodAgentStart, params, &snapshot)
	return snapshot, err
}

// AgentSnapshot reads one session.
func (c *Client) AgentSnapshot(ctx context.Context, sessionID string) (AgentSnapshot, error) {
	var snapshot AgentSnapshot
	err := c.CallInto(ctx, MethodAgentSnapshot, SessionParams{SessionID: sessionID}, &snapshot)
	return snapshot, err
}

// AgentList reads every session.
func (c *Client) AgentList(ctx context.Context) ([]AgentSnapshot, error) {
	var result struct {
		Sessions []AgentSnapshot `json:"sessions"`
	}
	err := c.CallInto(ctx, MethodAgentList, nil, &result)
	return result.Sessions, err
}

// Prompt sends text to a session.
func (c *Client) Prompt(ctx context.Context, sessionID, text string) error {
	return c.CallInto(ctx, MethodAgentPrompt, PromptParams{SessionID: sessionID, Text: text}, nil)
}

// Abort cancels the current turn.
func (c *Client) Abort(ctx context.Context, sessionID string) error {
	return c.CallInto(ctx, MethodAgentAbort, SessionParams{SessionID: sessionID}, nil)
}

// Stop ends the Agent process by explicit user intent.
func (c *Client) Stop(ctx context.Context, sessionID string) error {
	return c.CallInto(ctx, MethodAgentStop, SessionParams{SessionID: sessionID}, nil)
}

// Approve answers an approval request.
func (c *Client) Approve(ctx context.Context, params ApproveParams) error {
	return c.CallInto(ctx, MethodAgentApprove, params, nil)
}

// SetWorkspace mirrors Desktop metadata for remote clients.
func (c *Client) SetWorkspace(ctx context.Context, snapshot json.RawMessage) error {
	return c.CallInto(ctx, MethodWorkspaceSet, WorkspaceParams{Snapshot: snapshot}, nil)
}

// Workspace reads the mirrored metadata.
func (c *Client) Workspace(ctx context.Context) (json.RawMessage, error) {
	return c.Call(ctx, MethodWorkspaceGet, nil)
}

// RemoteStatus reads pairing state.
func (c *Client) RemoteStatus(ctx context.Context) (Status, error) {
	var status Status
	err := c.CallInto(ctx, MethodRemoteStatus, nil, &status)
	return status, err
}

// Invite creates a one-time pairing offer.
func (c *Client) Invite(ctx context.Context) (Invite, error) {
	var invite Invite
	err := c.CallInto(ctx, MethodRemoteInvite, nil, &invite)
	return invite, err
}

// Confirm answers a pending pairing prompt.
func (c *Client) Confirm(ctx context.Context, approve bool) error {
	return c.CallInto(ctx, MethodRemoteConfirm, ConfirmParams{Approve: approve}, nil)
}

// Revoke removes a device.
func (c *Client) Revoke(ctx context.Context, deviceID string) error {
	return c.CallInto(ctx, MethodRemoteRevoke, RevokeParams{DeviceID: deviceID}, nil)
}

// Grant adds an explicit device capability.
func (c *Client) Grant(ctx context.Context, params GrantParams) error {
	return c.CallInto(ctx, MethodRemoteGrant, params, nil)
}

// ScreenStart opens a media session.
func (c *Client) ScreenStart(ctx context.Context, params ScreenStartParams) (ScreenSession, error) {
	var session ScreenSession
	err := c.CallInto(ctx, MethodScreenStart, params, &session)
	return session, err
}

// ScreenAnswer completes negotiation.
func (c *Client) ScreenAnswer(ctx context.Context, sessionID, sdp string) error {
	return c.CallInto(ctx, MethodScreenAnswer, ScreenAnswerParams{SessionID: sessionID, SDP: sdp}, nil)
}

// ScreenStop ends capture.
func (c *Client) ScreenStop(ctx context.Context, sessionID string) error {
	return c.CallInto(ctx, MethodScreenStop, SessionParams{SessionID: sessionID}, nil)
}

// ScreenInput injects input under a control grant.
func (c *Client) ScreenInput(ctx context.Context, params ScreenInputParams) error {
	return c.CallInto(ctx, MethodScreenInput, params, nil)
}

// RepoStatus reads repository status for a remote client.
func (c *Client) RepoStatus(ctx context.Context, sessionID string) (json.RawMessage, error) {
	return c.Call(ctx, MethodRepoStatus, RepoParams{SessionID: sessionID})
}

// RepoDiff reads a unified patch for a remote client.
func (c *Client) RepoDiff(ctx context.Context, sessionID, path string, staged bool) (json.RawMessage, error) {
	return c.Call(ctx, MethodRepoDiff, RepoParams{SessionID: sessionID, Path: path, Staged: staged})
}

// Plugins lists installed extensions.
func (c *Client) Plugins(ctx context.Context) ([]extensions.PluginStatus, error) {
	var result struct {
		Plugins []extensions.PluginStatus `json:"plugins"`
	}
	err := c.CallInto(ctx, MethodPluginList, nil, &result)
	return result.Plugins, err
}

// InstallPlugin validates and registers a plugin directory.
func (c *Client) InstallPlugin(ctx context.Context, dir string) (extensions.PluginStatus, error) {
	var status extensions.PluginStatus
	err := c.CallInto(ctx, MethodPluginInstall, PluginInstallParams{Dir: dir}, &status)
	return status, err
}

// EnablePlugin starts an installed plugin.
func (c *Client) EnablePlugin(ctx context.Context, id string) error {
	return c.CallInto(ctx, MethodPluginEnable, PluginParams{Plugin: id}, nil)
}

// DisablePlugin stops a plugin.
func (c *Client) DisablePlugin(ctx context.Context, id string) error {
	return c.CallInto(ctx, MethodPluginDisable, PluginParams{Plugin: id}, nil)
}

// Events subscribes to pushed snapshots. The returned cancel must be called.
func (c *Client) Events(ctx context.Context, sessionID string) (<-chan Event, error) {
	url := c.base + "/v1/events"
	if sessionID != "" {
		url += "?session_id=" + sessionID
	}
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return nil, err
	}
	request.Header.Set("Authorization", "Bearer "+c.token)
	response, err := c.http.Do(request)
	if err != nil {
		return nil, err
	}
	if response.StatusCode != http.StatusOK {
		response.Body.Close()
		return nil, fmt.Errorf("bridge: events status %s", response.Status)
	}
	events := make(chan Event, 32)
	go func() {
		defer close(events)
		defer response.Body.Close()
		scanner := bufio.NewScanner(response.Body)
		scanner.Buffer(make([]byte, 64<<10), 4<<20)
		var data strings.Builder
		for scanner.Scan() {
			line := scanner.Text()
			switch {
			case strings.HasPrefix(line, "data: "):
				if data.Len() > 0 {
					data.WriteByte('\n')
				}
				data.WriteString(strings.TrimPrefix(line, "data: "))
			case line == "":
				if data.Len() == 0 {
					continue
				}
				var event Event
				if err := json.Unmarshal([]byte(data.String()), &event); err == nil {
					select {
					case events <- event:
					case <-ctx.Done():
						return
					}
				}
				data.Reset()
			}
		}
	}()
	return events, nil
}

func newRequestID() string {
	var raw [8]byte
	if _, err := rand.Read(raw[:]); err != nil {
		return fmt.Sprintf("%d", time.Now().UnixNano())
	}
	return hex.EncodeToString(raw[:])
}

// bridgeBinary resolves the bridge executable.
func bridgeBinary() (string, []string, error) {
	if configured := strings.TrimSpace(os.Getenv("UKK_BRIDGE_BIN")); configured != "" {
		return configured, nil, nil
	}
	self, err := os.Executable()
	if err != nil {
		return "", nil, err
	}
	name := "ukk-bridge"
	if strings.HasSuffix(strings.ToLower(self), ".exe") {
		name += ".exe"
	}
	sibling := filepath.Join(filepath.Dir(self), name)
	if info, statErr := os.Stat(sibling); statErr == nil && !info.IsDir() {
		return sibling, nil, nil
	}
	// The Desktop binary can host the bridge itself; packaging may not ship a
	// second executable.
	return self, []string{"--bridge"}, nil
}

// Detached starts the bridge without tying it to the caller's lifetime.
func StartDetached(dir string) error {
	command, args, err := bridgeBinary()
	if err != nil {
		return err
	}
	if len(args) == 0 {
		args = []string{"serve"}
	}
	cmd := exec.Command(command, args...)
	cmd.Env = append(os.Environ(), "UKK_DATA_DIR="+dir)
	logPath := filepath.Join(dir, "bridge.log")
	if logFile, err := os.OpenFile(logPath, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o600); err == nil {
		cmd.Stdout = logFile
		cmd.Stderr = logFile
	}
	detach(cmd)
	if err := cmd.Start(); err != nil {
		return fmt.Errorf("bridge: start: %w", err)
	}
	go func() { _ = cmd.Wait() }()
	return nil
}
