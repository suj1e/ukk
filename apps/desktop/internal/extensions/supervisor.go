package extensions

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"runtime"
	"sort"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

// Plugin runtime states.
const (
	StateDisabled = "disabled"
	StateStarting = "starting"
	StateReady    = "ready"
	StateFailed   = "failed"
)

const (
	defaultCallTimeout    = 5 * time.Second
	maxCallTimeout        = 10 * time.Second
	shutdownGrace         = 300 * time.Millisecond
	maxHelperStderrBytes  = 8 << 10
	maxHelperArgs         = 16
	maxHelperArgBytes     = 1024
	invokeArgsFrameMargin = 4 << 10
)

var envNamePattern = regexp.MustCompile(`^[A-Za-z_][A-Za-z0-9_]*$`)

// InProcess is the trusted in-process implementation behind a builtin plugin.
// It receives the same request as a helper would and returns one JSON value.
type InProcess interface {
	Invoke(ctx context.Context, request InvokeRequest) (json.RawMessage, error)
}

// InvokeRequest asks the supervisor to run one declared command.
type InvokeRequest struct {
	Plugin    string          `json:"plugin"`
	Command   string          `json:"command"`
	SessionID string          `json:"session_id,omitempty"`
	Scope     string          `json:"scope,omitempty"`
	Args      json.RawMessage `json:"args,omitempty"`
}

// InvokeResult is a command outcome with its resolved scope. Value is
// helper-authored or builtin-authored JSON and never a grant or ticket.
type InvokeResult struct {
	Plugin  string          `json:"plugin"`
	Command string          `json:"command"`
	Scope   string          `json:"scope"`
	Value   json.RawMessage `json:"value"`
}

// InstallOptions carries per-install inputs. Dir is the plugin directory; for
// helpers it must contain the entrypoint program. Env lists the only extra
// environment variables the helper receives, and Args is the helper argv after
// the program name.
type InstallOptions struct {
	Dir  string
	Env  map[string]string
	Args []string
}

// Config configures a Supervisor. Zero values are safe: an in-memory grant
// store, a 512-record audit ring, and a 5-second call deadline.
type Config struct {
	Root     string
	Grants   *GrantStore
	Audit    *Audit
	Builtins map[string]InProcess
	// CallTimeout is clamped to at most 10 seconds.
	CallTimeout time.Duration
}

// PluginStatus is a read-only lifecycle projection.
type PluginStatus struct {
	ID       string `json:"id"`
	Name     string `json:"name"`
	Version  string `json:"version"`
	Trust    string `json:"trust"`
	State    string `json:"state"`
	PID      int    `json:"pid,omitempty"`
	Commands int    `json:"commands"`
	Views    int    `json:"views"`
	Events   int    `json:"events"`
}

// Supervisor owns installation, enablement, grants/dispatch and helper
// lifecycles. It never scans a directory and never starts anything on install.
type Supervisor struct {
	mu            sync.Mutex
	root          string
	plugins       map[string]*pluginState
	builtins      map[string]InProcess
	grants        *GrantStore
	audit         *Audit
	callTimeout   time.Duration
	nextRequestID atomic.Uint64
}

type pluginState struct {
	manifest   *ValidatedManifest
	dir        string
	helperPath string
	args       []string
	env        map[string]string
	builtin    InProcess

	// opMu serialises all lifecycle and dispatch work for one plugin.
	opMu   sync.Mutex
	status string
	helper *helperProc
}

// NewSupervisor builds a supervisor from configuration.
func NewSupervisor(cfg Config) *Supervisor {
	grants := cfg.Grants
	if grants == nil {
		// OpenGrantStore cannot fail for the in-memory path.
		grants, _ = OpenGrantStore("")
	}
	audit := cfg.Audit
	if audit == nil {
		audit = NewAudit(DefaultAuditCapacity)
	}
	timeout := cfg.CallTimeout
	if timeout <= 0 {
		timeout = defaultCallTimeout
	}
	if timeout > maxCallTimeout {
		timeout = maxCallTimeout
	}
	builtins := make(map[string]InProcess, len(cfg.Builtins))
	for id, impl := range cfg.Builtins {
		if impl != nil {
			builtins[id] = impl
		}
	}
	return &Supervisor{
		root:        cfg.Root,
		plugins:     make(map[string]*pluginState),
		builtins:    builtins,
		grants:      grants,
		audit:       audit,
		callTimeout: timeout,
	}
}

// Grants returns the grant store used by dispatch.
func (s *Supervisor) Grants() *GrantStore { return s.grants }

// Audit returns the audit ring.
func (s *Supervisor) Audit() *Audit { return s.audit }

// RegisterBuiltin registers an in-process implementation for a builtin
// manifest. It must exist before that manifest is installed.
func (s *Supervisor) RegisterBuiltin(pluginID string, impl InProcess) error {
	if err := validatePluginID(pluginID); err != nil {
		return err
	}
	if impl == nil {
		return errf(CodeInvalidHelper, "builtin implementation is nil")
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	s.builtins[pluginID] = impl
	return nil
}

// InstallDir loads plugin.json from dir, validates it, and installs the plugin
// disabled. It starts no process and creates no grant.
func (s *Supervisor) InstallDir(dir string, opts InstallOptions) (*ValidatedManifest, error) {
	absDir, err := filepath.Abs(dir)
	if err != nil {
		return nil, errf(CodeInvalidHelper, "resolve %s: %s", dir, err.Error())
	}
	manifest, err := LoadManifest(filepath.Join(absDir, "plugin.json"))
	if err != nil {
		s.record("", ActionInstall, "", "", OutcomeError, err.Error())
		return nil, err
	}
	opts.Dir = absDir
	if err := s.Install(manifest, "", opts); err != nil {
		return nil, err
	}
	return manifest, nil
}

// Install registers a validated manifest. For helper plugins the executed
// program's file stem must equal the manifest entrypoint, and it must live
// inside the plugin directory. For builtin plugins a registered InProcess
// implementation is required and no program runs.
func (s *Supervisor) Install(manifest *ValidatedManifest, helperPath string, opts InstallOptions) error {
	if manifest == nil {
		return errf(CodeInvalidManifest, "manifest is required")
	}
	id := manifest.ID

	dir := opts.Dir
	if dir != "" {
		absDir, err := filepath.Abs(dir)
		if err != nil {
			return errf(CodeInvalidHelper, "resolve plugin directory: %s", err.Error())
		}
		dir = filepath.Clean(absDir)
	}
	env, err := normalizeEnv(opts.Env)
	if err != nil {
		s.record(id, ActionInstall, "", "", OutcomeDenied, err.Error())
		return err
	}
	if len(opts.Args) > maxHelperArgs {
		return errf(CodeInvalidArgs, "helper argv exceeds %d entries", maxHelperArgs)
	}
	for _, arg := range opts.Args {
		if len(arg) > maxHelperArgBytes {
			return errf(CodeInvalidArgs, "helper argument exceeds %d bytes", maxHelperArgBytes)
		}
	}
	state := &pluginState{
		manifest: manifest,
		dir:      dir,
		args:     append([]string(nil), opts.Args...),
		env:      env,
		status:   StateDisabled,
	}

	switch manifest.Trust {
	case TrustBuiltin:
		s.mu.Lock()
		impl := s.builtins[id]
		s.mu.Unlock()
		if impl == nil {
			s.record(id, ActionInstall, "", "", OutcomeDenied, "no builtin implementation registered")
			return errf(CodeInvalidHelper, "no in-process implementation registered for %q", id)
		}
		state.builtin = impl
	default:
		if helperPath == "" {
			if dir == "" {
				return errf(CodeInvalidHelper, "helper path is required for trust %q", manifest.Trust)
			}
			helperPath = filepath.Join(dir, manifest.Entrypoint)
		}
		absHelper, err := filepath.Abs(helperPath)
		if err != nil {
			return errf(CodeInvalidHelper, "resolve helper: %s", err.Error())
		}
		absHelper = filepath.Clean(absHelper)
		if dir == "" {
			dir = filepath.Dir(absHelper)
			state.dir = dir
		}
		rel, err := filepath.Rel(dir, absHelper)
		if err != nil || filepath.IsAbs(rel) || rel == ".." || strings.HasPrefix(rel, ".."+string(os.PathSeparator)) {
			return errf(CodeInvalidHelper, "helper %s is outside the plugin directory", absHelper)
		}
		info, err := os.Stat(absHelper)
		if err != nil || !info.Mode().IsRegular() {
			return errf(CodeInvalidHelper, "helper %s is not a regular file", absHelper)
		}
		if runtime.GOOS != "windows" && info.Mode().Perm()&0o111 == 0 {
			return errf(CodeInvalidHelper, "helper %s is not executable", absHelper)
		}
		if !entrypointMatches(manifest.Entrypoint, absHelper) {
			return errf(CodeInvalidHelper, "helper %q does not match entrypoint %q", filepath.Base(absHelper), manifest.Entrypoint)
		}
		state.helperPath = absHelper
	}

	s.mu.Lock()
	defer s.mu.Unlock()
	if _, exists := s.plugins[id]; exists {
		s.record(id, ActionInstall, "", "", OutcomeDenied, "already installed")
		return errf(CodeAlreadyInstalled, "plugin %q is already installed", id)
	}
	s.plugins[id] = state
	s.record(id, ActionInstall, "", "", OutcomeSuccess, "trust="+manifest.Trust)
	return nil
}

func normalizeEnv(in map[string]string) (map[string]string, error) {
	out := make(map[string]string, len(in))
	for name, value := range in {
		if !envNamePattern.MatchString(name) {
			return nil, errf(CodeInvalidArgs, "invalid environment variable name %q", name)
		}
		switch name {
		case "PATH", "HOME", "TMPDIR":
			return nil, errf(CodeInvalidArgs, "environment variable %s is reserved", name)
		}
		if strings.ContainsRune(value, 0) {
			return nil, errf(CodeInvalidArgs, "environment variable %s contains NUL", name)
		}
		out[name] = value
	}
	return out, nil
}

func entrypointMatches(entrypoint, helperPath string) bool {
	base := filepath.Base(helperPath)
	if base == entrypoint {
		return true
	}
	return strings.TrimSuffix(base, filepath.Ext(base)) == entrypoint
}

// Enable starts the helper (or binds the builtin implementation) and performs
// the hello handshake. Enabling never creates a grant.
func (s *Supervisor) Enable(ctx context.Context, pluginID string) error {
	if ctx == nil {
		ctx = context.Background()
	}
	state, err := s.lookup(pluginID)
	if err != nil {
		s.record(pluginID, ActionEnable, "", "", OutcomeDenied, "unknown plugin")
		return err
	}
	state.opMu.Lock()
	defer state.opMu.Unlock()
	if !s.installed(state) {
		err := errf(CodeUnknownPlugin, "plugin %q is not installed", pluginID)
		s.record(pluginID, ActionEnable, "", "", OutcomeDenied, err.Error())
		return err
	}
	if state.status == StateReady {
		return nil
	}
	state.status = StateStarting
	if state.builtin != nil {
		state.status = StateReady
		s.record(pluginID, ActionEnable, "", "", OutcomeSuccess, "trust=builtin")
		return nil
	}
	if err := s.startHelper(state); err != nil {
		state.status = StateFailed
		s.record(pluginID, ActionEnable, "", "", OutcomeError, err.Error())
		return err
	}
	if err := s.hello(ctx, state); err != nil {
		if state.helper != nil {
			state.helper.terminate()
			state.helper = nil
		}
		state.status = StateFailed
		s.record(pluginID, ActionEnable, "", "", OutcomeError, err.Error())
		return err
	}
	state.status = StateReady
	s.record(pluginID, ActionEnable, "", "", OutcomeSuccess, "trust="+state.manifest.Trust)
	return nil
}

func (s *Supervisor) hello(ctx context.Context, state *pluginState) error {
	helper := state.helper
	if helper == nil {
		return errf(CodeProtocolViolation, "helper is not running")
	}
	callCtx, cancel := context.WithTimeout(ctx, s.callTimeout)
	defer cancel()
	response, err := helper.call(callCtx, Request{
		ID:       1,
		Method:   MethodHello,
		PluginID: state.manifest.ID,
		Protocol: ProtocolVersion,
	}, s.callTimeout)
	if err != nil {
		return err
	}
	if response.ID != 1 {
		return errf(CodeProtocolViolation, "hello response id %d, want 1", response.ID)
	}
	if !response.OK {
		return errf(CodeProtocolViolation, "hello was refused by the helper")
	}
	var hello HelloResult
	if err := decodeStrict(response.Result, &hello); err != nil {
		return errf(CodeProtocolViolation, "invalid hello result: %s", Redact(err.Error()))
	}
	if hello.Protocol != ProtocolVersion {
		return errf(CodeProtocolViolation, "helper protocol %d, want %d", hello.Protocol, ProtocolVersion)
	}
	if hello.PluginID != state.manifest.ID {
		return errf(CodeProtocolViolation, "helper identity %q does not match manifest", hello.PluginID)
	}
	for _, id := range hello.Commands {
		if _, ok := state.manifest.Command(id); !ok {
			return errf(CodeProtocolViolation, "helper reported undeclared command %q", id)
		}
	}
	for _, id := range hello.Views {
		if _, ok := state.manifest.View(id); !ok {
			return errf(CodeProtocolViolation, "helper reported undeclared view %q", id)
		}
	}
	for _, id := range hello.Events {
		if _, ok := state.manifest.Event(id); !ok {
			return errf(CodeProtocolViolation, "helper reported undeclared event %q", id)
		}
	}
	return nil
}

// Disable stops the helper (shutdown frame, then kill after the grace period)
// and marks the plugin disabled. It never revokes grants.
func (s *Supervisor) Disable(pluginID string) error {
	state, err := s.lookup(pluginID)
	if err != nil {
		s.record(pluginID, ActionDisable, "", "", OutcomeDenied, "unknown plugin")
		return err
	}
	state.opMu.Lock()
	defer state.opMu.Unlock()
	if !s.installed(state) {
		err := errf(CodeUnknownPlugin, "plugin %q is not installed", pluginID)
		s.record(pluginID, ActionDisable, "", "", OutcomeDenied, err.Error())
		return err
	}
	if state.status == StateDisabled {
		return nil
	}
	state.stop(shutdownGrace)
	s.record(pluginID, ActionDisable, "", "", OutcomeSuccess, "helper stopped")
	return nil
}

// Uninstall stops the helper, revokes every grant and removes the plugin
// directory for helper installs. Command ids are freed with the registry entry.
func (s *Supervisor) Uninstall(pluginID string) error {
	s.mu.Lock()
	state, ok := s.plugins[pluginID]
	if ok {
		delete(s.plugins, pluginID)
	}
	s.mu.Unlock()
	if !ok {
		s.record(pluginID, ActionUninstall, "", "", OutcomeDenied, "unknown plugin")
		return errf(CodeUnknownPlugin, "plugin %q is not installed", pluginID)
	}

	state.opMu.Lock()
	state.stop(shutdownGrace)
	state.opMu.Unlock()

	var failures []error
	if _, err := s.grants.RevokeAll(pluginID); err != nil {
		failures = append(failures, err)
	}
	removed := false
	if state.manifest.Trust != TrustBuiltin && state.dir != "" {
		if err := s.removeDir(state.dir); err != nil {
			failures = append(failures, err)
		} else {
			removed = true
		}
	}
	if len(failures) > 0 {
		joined := errors.Join(failures...)
		s.record(pluginID, ActionUninstall, "", "", OutcomeError, joined.Error())
		return joined
	}
	s.record(pluginID, ActionUninstall, "", "", OutcomeSuccess, fmt.Sprintf("dir_removed=%t", removed))
	return nil
}

func (s *Supervisor) removeDir(dir string) error {
	abs, err := filepath.Abs(dir)
	if err != nil {
		return errf(CodeInvalidHelper, "resolve plugin directory: %s", err.Error())
	}
	abs = filepath.Clean(abs)
	if abs == string(os.PathSeparator) || abs == "." {
		return errf(CodeInvalidHelper, "refusing to remove %s", abs)
	}
	if s.root != "" {
		root, err := filepath.Abs(s.root)
		if err != nil {
			return errf(CodeInvalidHelper, "resolve plugin root: %s", err.Error())
		}
		root = filepath.Clean(root)
		rel, err := filepath.Rel(root, abs)
		if err != nil || filepath.IsAbs(rel) || rel == "." || rel == ".." || strings.HasPrefix(rel, ".."+string(os.PathSeparator)) {
			return errf(CodeInvalidHelper, "refusing to remove %s outside %s", abs, root)
		}
	}
	if err := os.RemoveAll(abs); err != nil {
		return errf(CodeInvalidHelper, "remove plugin directory: %s", err.Error())
	}
	return nil
}

// Revoke revokes one grant and audits the decision.
func (s *Supervisor) Revoke(plugin, capability, scope string) (bool, error) {
	revoked, err := s.grants.Revoke(plugin, capability, scope)
	if err != nil {
		s.record(plugin, ActionRevoke, capability, scope, OutcomeError, err.Error())
		return false, err
	}
	detail := "revoked"
	if !revoked {
		detail = "no matching grant"
	}
	s.record(plugin, ActionRevoke, capability, scope, OutcomeSuccess, detail)
	return revoked, nil
}

// Invoke dispatches one declared command. Ownership, readiness, session scope,
// every required grant and argument size are checked before any helper frame
// is sent. Success and every denial produce an audit record.
func (s *Supervisor) Invoke(ctx context.Context, request InvokeRequest) (InvokeResult, error) {
	if ctx == nil {
		ctx = context.Background()
	}
	result := InvokeResult{Plugin: request.Plugin, Command: request.Command}
	if request.Plugin == "" {
		return result, s.denyInvoke(request, "", "", CodeInvalidArgs, "plugin id is required")
	}
	state, err := s.lookup(request.Plugin)
	if err != nil {
		s.record(request.Plugin, ActionInvoke, "", "", OutcomeDenied, "unknown plugin")
		return result, err
	}
	state.opMu.Lock()
	defer state.opMu.Unlock()
	if !s.installed(state) {
		return result, s.denyInvoke(request, "", "", CodeUnknownPlugin, "plugin is not installed")
	}
	if request.Command == "" {
		return result, s.denyInvoke(request, "", "", CodeInvalidArgs, "command id is required")
	}
	command, ok := state.manifest.Command(request.Command)
	if !ok {
		return result, s.denyInvoke(request, "", "", CodeUnknownCommand, fmt.Sprintf("command %q is not declared", request.Command))
	}
	if state.status != StateReady {
		return result, s.denyInvoke(request, "", "", CodeNotEnabled, "plugin is not enabled")
	}

	scope, sessionID, err := resolveInvokeScope(command, request)
	if err != nil {
		return result, s.denyInvoke(request, "", "", CodeOf(err), err.(*Error).Message)
	}
	result.Scope = scope

	now := time.Now()
	for _, capability := range command.Requires {
		if !s.grants.Authorized(request.Plugin, capability, scope, now) {
			return result, s.denyInvoke(request, capability, scope, CodeCapabilityDenied,
				fmt.Sprintf("missing grant for %s in scope %s", capability, scope))
		}
	}

	if len(request.Args) > MaxFrameBytes-invokeArgsFrameMargin {
		return result, s.denyInvoke(request, strings.Join(command.Requires, ","), scope, CodeInvalidArgs, "arguments exceed the helper frame budget")
	}
	if len(request.Args) > 0 && !json.Valid(request.Args) {
		return result, s.denyInvoke(request, strings.Join(command.Requires, ","), scope, CodeInvalidArgs, "arguments are not valid JSON")
	}

	dispatch := request
	dispatch.Scope = scope
	dispatch.SessionID = sessionID
	capabilitySummary := strings.Join(command.Requires, ",")

	if state.builtin != nil {
		callCtx, cancel := context.WithTimeout(ctx, s.callTimeout)
		value, err := state.builtin.Invoke(callCtx, dispatch)
		cancel()
		if err != nil {
			s.record(request.Plugin, ActionInvoke, capabilitySummary, scope, OutcomeError, "builtin failed: "+err.Error())
			return result, errf(CodeHelperFailed, "plugin %q failed: %s", request.Plugin, Redact(err.Error()))
		}
		if len(value) == 0 {
			value = json.RawMessage("null")
		}
		result.Value = value
		s.record(request.Plugin, ActionInvoke, capabilitySummary, scope, OutcomeSuccess,
			fmt.Sprintf("command=%s scope=%s", command.ID, scope))
		return result, nil
	}

	helper := state.helper
	if helper == nil {
		state.status = StateFailed
		return result, s.denyInvoke(request, capabilitySummary, scope, CodeNotEnabled, "helper is not running")
	}
	requestID := s.nextRequestID.Add(1)
	callCtx, cancel := context.WithTimeout(ctx, s.callTimeout)
	response, callErr := helper.call(callCtx, Request{
		ID:      requestID,
		Method:  MethodInvoke,
		Command: command.ID,
		Session: sessionID,
		Args:    request.Args,
	}, s.callTimeout)
	cancel()
	if callErr != nil {
		state.helper = nil
		helper.terminate()
		state.status = StateFailed
		code := CodeOf(callErr)
		if code == "" {
			code = CodeHelperFailed
		}
		s.record(request.Plugin, ActionInvoke, capabilitySummary, scope, OutcomeError,
			fmt.Sprintf("dispatch failed code=%s detail=%s", code, Redact(callErr.Error())))
		return result, callErr
	}
	if response.ID != requestID {
		state.helper = nil
		helper.terminate()
		state.status = StateFailed
		err := errf(CodeProtocolViolation, "response id %d does not match request %d", response.ID, requestID)
		s.record(request.Plugin, ActionInvoke, capabilitySummary, scope, OutcomeError, err.Error())
		return result, err
	}
	if !response.OK {
		detail := "helper refused the command"
		if response.Error != nil {
			detail = fmt.Sprintf("code=%s message=%s", response.Error.Code, response.Error.Message)
		}
		s.record(request.Plugin, ActionInvoke, capabilitySummary, scope, OutcomeDenied, detail)
		return result, errf(CodeHelperError, "%s", Redact(detail))
	}
	value := response.Result
	if len(value) == 0 {
		value = json.RawMessage("null")
	}
	result.Value = value
	s.record(request.Plugin, ActionInvoke, capabilitySummary, scope, OutcomeSuccess,
		fmt.Sprintf("command=%s scope=%s", command.ID, scope))
	return result, nil
}

func resolveInvokeScope(command Command, request InvokeRequest) (scope, sessionID string, err error) {
	sessionID = request.SessionID
	scope = request.Scope
	if sessionID != "" {
		if len(sessionID) > MaxIDLength || !contributionPattern.MatchString(sessionID) || strings.Contains(sessionID, "..") {
			return "", "", errf(CodeInvalidArgs, "session id is invalid")
		}
		want := SessionScope(sessionID)
		if scope != "" && scope != want {
			return "", "", errf(CodeInvalidArgs, "scope does not match the supplied session")
		}
		scope = want
	}
	if scope == "" {
		scope = ScopeHost
	}
	if err := ValidateScope(scope); err != nil {
		return "", "", errf(CodeInvalidArgs, "%s", err.(*Error).Message)
	}
	if strings.HasPrefix(scope, ScopeSessionPrefix) && sessionID == "" {
		sessionID = strings.TrimPrefix(scope, ScopeSessionPrefix)
	}
	if command.Scope == CommandScopeSession && !strings.HasPrefix(scope, ScopeSessionPrefix) {
		return "", "", errf(CodeMissingSession, "command %q requires a session scope", command.ID)
	}
	return scope, sessionID, nil
}

func (s *Supervisor) denyInvoke(request InvokeRequest, capability, scope, code, detail string) error {
	s.record(request.Plugin, ActionInvoke, capability, scope, OutcomeDenied, detail)
	return errf(code, "%s", detail)
}

func (s *Supervisor) record(plugin, action, capability, scope, outcome, detail string) {
	s.audit.Record(AuditRecord{
		Plugin:     plugin,
		Action:     action,
		Capability: capability,
		Scope:      scope,
		Outcome:    outcome,
		Detail:     detail,
	})
}

func (s *Supervisor) lookup(pluginID string) (*pluginState, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	state, ok := s.plugins[pluginID]
	if !ok {
		return nil, errf(CodeUnknownPlugin, "plugin %q is not installed", pluginID)
	}
	return state, nil
}

func (s *Supervisor) installed(state *pluginState) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	current, ok := s.plugins[state.manifest.ID]
	return ok && current == state
}

// Status returns the current lifecycle projection for one plugin.
func (s *Supervisor) Status(pluginID string) (PluginStatus, error) {
	state, err := s.lookup(pluginID)
	if err != nil {
		return PluginStatus{}, err
	}
	state.opMu.Lock()
	defer state.opMu.Unlock()
	status := PluginStatus{
		ID:       state.manifest.ID,
		Name:     state.manifest.Name,
		Version:  state.manifest.Version,
		Trust:    state.manifest.Trust,
		State:    state.status,
		Commands: len(state.manifest.commands),
		Views:    len(state.manifest.views),
		Events:   len(state.manifest.events),
	}
	if state.helper != nil && state.helper.cmd.Process != nil {
		status.PID = state.helper.cmd.Process.Pid
	}
	return status, nil
}

// List returns statuses for every installed plugin, sorted by id.
func (s *Supervisor) List() []PluginStatus {
	s.mu.Lock()
	states := make([]*pluginState, 0, len(s.plugins))
	for _, state := range s.plugins {
		states = append(states, state)
	}
	s.mu.Unlock()
	statuses := make([]PluginStatus, 0, len(states))
	for _, state := range states {
		state.opMu.Lock()
		status := PluginStatus{
			ID:       state.manifest.ID,
			Name:     state.manifest.Name,
			Version:  state.manifest.Version,
			Trust:    state.manifest.Trust,
			State:    state.status,
			Commands: len(state.manifest.commands),
			Views:    len(state.manifest.views),
			Events:   len(state.manifest.events),
		}
		if state.helper != nil && state.helper.cmd.Process != nil {
			status.PID = state.helper.cmd.Process.Pid
		}
		state.opMu.Unlock()
		statuses = append(statuses, status)
	}
	sort.Slice(statuses, func(i, j int) bool { return statuses[i].ID < statuses[j].ID })
	return statuses
}

// Shutdown stops every running helper. It leaves installation and grants
// intact; process teardown of the app does not uninstall plugins.
func (s *Supervisor) Shutdown() {
	s.mu.Lock()
	states := make([]*pluginState, 0, len(s.plugins))
	for _, state := range s.plugins {
		states = append(states, state)
	}
	s.mu.Unlock()
	for _, state := range states {
		state.opMu.Lock()
		state.stop(shutdownGrace)
		state.opMu.Unlock()
	}
}

func (s *Supervisor) startHelper(state *pluginState) error {
	command := exec.Command(state.helperPath, state.args...)
	command.Dir = state.dir
	command.Env = buildHelperEnv(state.env)
	stdin, err := command.StdinPipe()
	if err != nil {
		return errf(CodeInvalidHelper, "open helper stdin: %s", err.Error())
	}
	stdout, err := command.StdoutPipe()
	if err != nil {
		stdin.Close()
		return errf(CodeInvalidHelper, "open helper stdout: %s", err.Error())
	}
	stderr := newBoundedBuffer(maxHelperStderrBytes)
	command.Stderr = stderr
	if err := command.Start(); err != nil {
		stdin.Close()
		return errf(CodeInvalidHelper, "start helper: %s", err.Error())
	}
	state.helper = &helperProc{
		cmd:    command,
		stdin:  stdin,
		stdout: bufio.NewReaderSize(stdout, 16<<10),
		stderr: stderr,
	}
	return nil
}

func buildHelperEnv(declared map[string]string) []string {
	env := make([]string, 0, 3+len(declared))
	for _, name := range []string{"PATH", "HOME", "TMPDIR"} {
		if value, ok := os.LookupEnv(name); ok {
			env = append(env, name+"="+value)
		}
	}
	names := make([]string, 0, len(declared))
	for name := range declared {
		names = append(names, name)
	}
	sort.Strings(names)
	for _, name := range names {
		env = append(env, name+"="+declared[name])
	}
	return env
}

func (state *pluginState) stop(grace time.Duration) {
	helper := state.helper
	state.helper = nil
	state.status = StateDisabled
	if helper != nil {
		helper.gracefulStop(grace)
	}
}

// helperProc is one running helper process with a single in-flight request.
type helperProc struct {
	cmd   *exec.Cmd
	stdin interface {
		Write([]byte) (int, error)
		Close() error
	}
	stdout *bufio.Reader
	stderr *boundedBuffer
	waited bool
}

func (h *helperProc) write(request Request) error {
	return WriteFrame(h.stdin, request)
}

type frameResult struct {
	payload []byte
	err     error
}

func (h *helperProc) call(ctx context.Context, request Request, timeout time.Duration) (*Response, error) {
	if err := h.write(request); err != nil {
		return nil, err
	}
	results := make(chan frameResult, 1)
	go func() {
		payload, err := ReadFrame(h.stdout)
		results <- frameResult{payload: payload, err: err}
	}()
	timer := time.NewTimer(timeout)
	defer timer.Stop()
	var payload []byte
	select {
	case result := <-results:
		if result.err != nil {
			return nil, result.err
		}
		payload = result.payload
	case <-timer.C:
		return nil, errf(CodeHelperTimeout, "helper did not answer within %s", timeout)
	case <-ctx.Done():
		return nil, ctx.Err()
	}
	var response Response
	if err := decodeStrict(payload, &response); err != nil {
		return nil, errf(CodeProtocolViolation, "invalid helper response: %s", Redact(err.Error()))
	}
	return &response, nil
}

func (h *helperProc) terminate() {
	_ = h.stdin.Close()
	if h.cmd.Process != nil {
		_ = h.cmd.Process.Kill()
	}
	h.reap()
}

func (h *helperProc) reap() {
	if h.waited {
		return
	}
	h.waited = true
	_ = h.cmd.Wait()
}

func (h *helperProc) gracefulStop(grace time.Duration) {
	_ = h.write(Request{Method: MethodShutdown})
	_ = h.stdin.Close()
	done := make(chan struct{})
	go func() {
		_ = h.cmd.Wait()
		close(done)
	}()
	select {
	case <-done:
	case <-time.After(grace):
		if h.cmd.Process != nil {
			_ = h.cmd.Process.Kill()
		}
		<-done
	}
	h.waited = true
}

// boundedBuffer keeps at most limit bytes of helper stderr and discards the
// rest without blocking the child. Its content is never logged as a payload.
type boundedBuffer struct {
	mu      sync.Mutex
	limit   int
	data    []byte
	dropped int
}

func newBoundedBuffer(limit int) *boundedBuffer {
	return &boundedBuffer{limit: limit}
}

func (b *boundedBuffer) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	remaining := b.limit - len(b.data)
	if remaining > 0 {
		if len(p) <= remaining {
			b.data = append(b.data, p...)
		} else {
			b.data = append(b.data, p[:remaining]...)
			b.dropped += len(p) - remaining
		}
	} else {
		b.dropped += len(p)
	}
	return len(p), nil
}

func (b *boundedBuffer) Bytes() []byte {
	b.mu.Lock()
	defer b.mu.Unlock()
	return append([]byte(nil), b.data...)
}

func (b *boundedBuffer) Dropped() int {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.dropped
}
