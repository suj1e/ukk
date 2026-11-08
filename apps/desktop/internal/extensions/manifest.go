// Package extensions implements the narrow ukk extension boundary: a strict
// JSON manifest, a capability/grant model with live expiry, a length-prefixed
// helper-process protocol, a supervisor that owns helper lifecycles, and a
// bounded redacted audit ring.
//
// It is deliberately not a general plugin framework. It never starts or stops
// a terminal or Agent, never infers approval from helper output, and exposes
// no shell or dynamic code loading for builtin plugins.
package extensions

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"regexp"
	"strconv"
	"strings"
	"unicode"
	"unicode/utf8"
)

// Package-wide limits. They bound everything a manifest or a helper can make
// the host allocate.
const (
	// ManifestVersion is the only accepted manifest schema version.
	ManifestVersion = 1
	// APIVersion is the extension API version this host implements.
	APIVersion = "0.1.0"
	// MaxFrameBytes is the largest helper frame, including its JSON payload.
	MaxFrameBytes = 64 << 10
	// MaxManifestBytes bounds a plugin.json file.
	MaxManifestBytes = 256 << 10

	MaxCommands            = 32
	MaxViews               = 32
	MaxEventSubscriptions  = 16
	MaxCapabilitiesPerList = 32
	MaxIDLength            = 64
	MaxTitleLength         = 80
	MaxDescriptionLength   = 240
	MaxEntrypointLength    = 255
	MaxAPIRangeBounds      = 8
	MinOverlayPixels       = 120
	MaxOverlayPixels       = 1080
)

// Trust levels. builtin runs in-process through InProcess; the other two run
// in a helper process with the same host-user identity.
const (
	TrustBuiltin    = "builtin"
	TrustFirstParty = "first-party"
	TrustThirdParty = "third-party"
)

// Contribution slots.
const (
	SlotSessionAction       = "session.action"
	SlotSessionTab          = "session.tab"
	SlotSessionOverlay      = "session.overlay"
	SlotSessionContextPanel = "session.context-panel"
	SlotSettingsSection     = "settings.section"
)

// Client platforms a view may target.
const (
	PlatformDesktop = "desktop"
	PlatformIOS     = "ios"
)

// Command scopes.
const (
	CommandScopeSession   = "session"
	CommandScopeWorkspace = "workspace"
	CommandScopeGlobal    = "global"
)

// Event kinds.
const (
	EventWorkspaceChanged = "workspace.changed"
	EventSessionUpdated   = "session.updated"
	EventAgentStatus      = "agent.status"
)

// Overlay anchors.
const (
	AnchorTopLeft     = "top-left"
	AnchorTopRight    = "top-right"
	AnchorBottomLeft  = "bottom-left"
	AnchorBottomRight = "bottom-right"
	AnchorCenter      = "center"
)

// Capability vocabulary.
const (
	CapWorkspaceRead     = "workspace.read"
	CapSessionObserve    = "session.observe"
	CapAgentSend         = "agent.send"
	CapAgentApprove      = "agent.approve"
	CapFilesystemRead    = "filesystem.read"
	CapFilesystemWrite   = "filesystem.write"
	CapGitWrite          = "git.write"
	CapScreenView        = "screen.view"
	CapScreenControl     = "screen.control"
	CapScreenAudio       = "screen.audio"
	CapNotificationsSend = "notifications.send"
	CapNetworkConnect    = "network.connect"
)

// Risk classes. Observe capabilities may be long-lived; mutate capabilities
// need an explicit grant with an optional TTL; sensitive capabilities require
// a short lease.
type Risk int

const (
	RiskObserve Risk = iota
	RiskMutate
	RiskSensitive
)

// String returns the stable lowercase risk name.
func (r Risk) String() string {
	switch r {
	case RiskObserve:
		return "observe"
	case RiskMutate:
		return "mutate"
	case RiskSensitive:
		return "sensitive"
	default:
		return "unknown"
	}
}

// Stable error codes. Callers branch on these, not on message text.
const (
	CodeInvalidManifest            = "invalid_manifest"
	CodeUnknownField               = "unknown_field"
	CodeUnsupportedManifestVersion = "unsupported_manifest_version"
	CodeInvalidID                  = "invalid_id"
	CodeInvalidName                = "invalid_name"
	CodeInvalidVersion             = "invalid_version"
	CodeInvalidAPIRange            = "invalid_api_range"
	CodeUnknownTrust               = "unknown_trust"
	CodeInvalidEntrypoint          = "invalid_entrypoint"
	CodeUnknownCapability          = "unknown_capability"
	CodeDuplicateID                = "duplicate_id"
	CodeUndeclaredCapability       = "undeclared_capability"
	CodeUnknownScope               = "unknown_scope"
	CodeUnknownSlot                = "unknown_slot"
	CodeUnknownEventKind           = "unknown_event_kind"
	CodeUnknownPlatform            = "unknown_platform"
	CodeInvalidOverlay             = "invalid_overlay"
	CodeLimitExceeded              = "limit_exceeded"

	CodeInvalidGrant       = "invalid_grant"
	CodeInvalidScope       = "invalid_scope"
	CodeAlreadyInstalled   = "already_installed"
	CodeUnknownPlugin      = "unknown_plugin"
	CodeUnknownCommand     = "unknown_command"
	CodeNotEnabled         = "not_enabled"
	CodeMissingSession     = "missing_session"
	CodeCapabilityDenied   = "capability_denied"
	CodeInvalidArgs        = "invalid_args"
	CodeInvalidHelper      = "invalid_helper"
	CodeHelperTimeout      = "helper_timeout"
	CodeHelperFailed       = "helper_failed"
	CodeHelperError        = "helper_error"
	CodeProtocolViolation  = "protocol_violation"
	CodeFrameTooLarge      = "frame_too_large"
	CodeInsecurePermission = "insecure_permission"
)

// Error is a typed extension-boundary error with a stable code.
type Error struct {
	Code    string
	Message string
}

// Error implements error.
func (e *Error) Error() string {
	if e.Message == "" {
		return e.Code
	}
	return e.Code + ": " + e.Message
}

func errf(code, format string, args ...any) *Error {
	return &Error{Code: code, Message: fmt.Sprintf(format, args...)}
}

// CodeOf returns the stable code of an Error, or "" for any other error.
func CodeOf(err error) string {
	var e *Error
	if errors.As(err, &e) {
		return e.Code
	}
	return ""
}

// IsCode reports whether err carries the given stable code.
func IsCode(err error, code string) bool {
	return CodeOf(err) == code
}

// Manifest is the strict JSON shape of plugin.json. It is decoded with
// DisallowUnknownFields, so adding a field requires a schema version bump.
type Manifest struct {
	ManifestVersion int           `json:"manifest_version"`
	ID              string        `json:"id"`
	Name            string        `json:"name"`
	Version         string        `json:"version"`
	API             string        `json:"api"`
	Trust           string        `json:"trust"`
	Entrypoint      string        `json:"entrypoint,omitempty"`
	Capabilities    Capabilities  `json:"capabilities"`
	Contributions   Contributions `json:"contributions"`
}

// Capabilities are declarations only. They never create a grant.
type Capabilities struct {
	Required []string `json:"required"`
	Optional []string `json:"optional"`
}

// Contributions describes what a plugin offers the UI and the event bus.
type Contributions struct {
	Commands []Command           `json:"commands"`
	Views    []View              `json:"views"`
	Events   []EventSubscription `json:"events"`
}

// Command is one invokable unit of work.
type Command struct {
	ID          string   `json:"id"`
	Title       string   `json:"title"`
	Description string   `json:"description,omitempty"`
	Scope       string   `json:"scope"`
	Requires    []string `json:"requires"`
}

// View is a native UI descriptor. It never carries UI code.
type View struct {
	ID        string   `json:"id"`
	Slot      string   `json:"slot"`
	Title     string   `json:"title"`
	Command   string   `json:"command,omitempty"`
	Platforms []string `json:"platforms"`
	Overlay   *Overlay `json:"overlay,omitempty"`
}

// Overlay bounds a floating Session contribution in absolute pixels. The
// renderer still clamps it to the hosting Session view.
type Overlay struct {
	Anchor    string `json:"anchor"`
	MaxWidth  int    `json:"max_width"`
	MaxHeight int    `json:"max_height"`
}

// EventSubscription is one declared subscription to host event kinds.
type EventSubscription struct {
	ID    string   `json:"id"`
	Kinds []string `json:"kinds"`
}

// ValidatedManifest is the only manifest type the supervisor accepts. Its
// maps are unexported so a partially validated manifest cannot be constructed
// from outside this package.
type ValidatedManifest struct {
	Manifest
	commands     map[string]Command
	views        map[string]View
	events       map[string]EventSubscription
	capabilities map[string]struct{}
}

// Command returns a declared command by id.
func (v *ValidatedManifest) Command(id string) (Command, bool) {
	c, ok := v.commands[id]
	return c, ok
}

// View returns a declared view by id.
func (v *ValidatedManifest) View(id string) (View, bool) {
	view, ok := v.views[id]
	return view, ok
}

// Event returns a declared event subscription by id.
func (v *ValidatedManifest) Event(id string) (EventSubscription, bool) {
	e, ok := v.events[id]
	return e, ok
}

// DeclaresCapability reports whether the manifest declared capability,
// required or optional. A declaration is not a grant.
func (v *ValidatedManifest) DeclaresCapability(capability string) bool {
	_, ok := v.capabilities[capability]
	return ok
}

// Commands returns a copy of the declared commands.
func (v *ValidatedManifest) Commands() []Command {
	out := make([]Command, 0, len(v.commands))
	for _, c := range v.Manifest.Contributions.Commands {
		out = append(out, c)
	}
	return out
}

// Views returns a copy of the declared views.
func (v *ValidatedManifest) Views() []View {
	out := make([]View, 0, len(v.views))
	for _, view := range v.Manifest.Contributions.Views {
		out = append(out, view)
	}
	return out
}

// Events returns a copy of the declared event subscriptions.
func (v *ValidatedManifest) Events() []EventSubscription {
	out := make([]EventSubscription, 0, len(v.events))
	for _, e := range v.Manifest.Contributions.Events {
		out = append(out, e)
	}
	return out
}

// LoadManifest reads and validates plugin.json with a hard size bound.
func LoadManifest(path string) (*ValidatedManifest, error) {
	data, err := readFileLimited(path, MaxManifestBytes)
	if err != nil {
		return nil, err
	}
	return ValidateManifest(data)
}

// ValidateManifest strictly decodes and validates a manifest document.
func ValidateManifest(data []byte) (*ValidatedManifest, error) {
	if len(data) == 0 {
		return nil, errf(CodeInvalidManifest, "manifest is empty")
	}
	if len(data) > MaxManifestBytes {
		return nil, errf(CodeLimitExceeded, "manifest exceeds %d bytes", MaxManifestBytes)
	}
	dec := json.NewDecoder(bytes.NewReader(data))
	dec.DisallowUnknownFields()
	var m Manifest
	if err := dec.Decode(&m); err != nil {
		if strings.Contains(err.Error(), "unknown field") {
			return nil, errf(CodeUnknownField, "%s", err.Error())
		}
		return nil, errf(CodeInvalidManifest, "%s", err.Error())
	}
	var extra json.RawMessage
	if err := dec.Decode(&extra); err != io.EOF {
		return nil, errf(CodeInvalidManifest, "trailing data after manifest")
	}
	return validateManifest(&m)
}

func validateManifest(m *Manifest) (*ValidatedManifest, error) {
	if m.ManifestVersion != ManifestVersion {
		return nil, errf(CodeUnsupportedManifestVersion, "manifest_version must be %d", ManifestVersion)
	}
	if err := validatePluginID(m.ID); err != nil {
		return nil, err
	}
	if err := validateTextField("name", m.Name, MaxTitleLength, true); err != nil {
		return nil, err
	}
	if err := validateNumericVersion(m.Version); err != nil {
		return nil, err
	}
	if err := validateAPIRange(m.API); err != nil {
		return nil, err
	}
	switch m.Trust {
	case TrustBuiltin, TrustFirstParty, TrustThirdParty:
	default:
		return nil, errf(CodeUnknownTrust, "trust must be builtin, first-party or third-party, got %q", m.Trust)
	}
	if err := validateEntrypoint(m.Trust, m.Entrypoint); err != nil {
		return nil, err
	}

	declared := make(map[string]struct{})
	for _, list := range [][]string{m.Capabilities.Required, m.Capabilities.Optional} {
		if len(list) > MaxCapabilitiesPerList {
			return nil, errf(CodeLimitExceeded, "capability list exceeds %d entries", MaxCapabilitiesPerList)
		}
		for _, capability := range list {
			if _, err := CapabilityRisk(capability); err != nil {
				return nil, err
			}
			if _, duplicate := declared[capability]; duplicate {
				return nil, errf(CodeDuplicateID, "duplicate capability %q", capability)
			}
			declared[capability] = struct{}{}
		}
	}

	commands, err := validateCommands(m.Contributions.Commands, declared)
	if err != nil {
		return nil, err
	}
	views, err := validateViews(m.Contributions.Views, commands)
	if err != nil {
		return nil, err
	}
	events, err := validateEvents(m.Contributions.Events, declared)
	if err != nil {
		return nil, err
	}
	return &ValidatedManifest{
		Manifest:     *m,
		commands:     commands,
		views:        views,
		events:       events,
		capabilities: declared,
	}, nil
}

var (
	pluginIDPattern     = regexp.MustCompile(`^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]*[a-z0-9])?)+$`)
	contributionPattern = regexp.MustCompile(`^[a-z0-9](?:[a-z0-9._-]*[a-z0-9])?$`)
	numericVersion      = regexp.MustCompile(`^[0-9]+\.[0-9]+\.[0-9]+$`)
	domainLabelPattern  = regexp.MustCompile(`^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$`)
)

func validatePluginID(id string) error {
	if len(id) == 0 || len(id) > MaxIDLength || !pluginIDPattern.MatchString(id) {
		return errf(CodeInvalidID, "plugin id %q must be lowercase reverse-DNS with at least two components and at most %d characters", id, MaxIDLength)
	}
	return nil
}

func validateContributionID(field, id string) error {
	if len(id) == 0 || len(id) > MaxIDLength || !contributionPattern.MatchString(id) || strings.Contains(id, "..") {
		return errf(CodeInvalidID, "%s id %q is invalid", field, id)
	}
	return nil
}

func validateTextField(field, value string, max int, required bool) error {
	if value == "" {
		if required {
			return errf(CodeInvalidName, "%s is required", field)
		}
		return nil
	}
	if utf8.RuneCountInString(value) > max {
		return errf(CodeInvalidName, "%s exceeds %d characters", field, max)
	}
	for _, r := range value {
		if unicode.IsControl(r) {
			return errf(CodeInvalidName, "%s contains control characters", field)
		}
	}
	return nil
}

func validateNumericVersion(version string) error {
	if !numericVersion.MatchString(version) {
		return errf(CodeInvalidVersion, "version %q must be numeric major.minor.patch", version)
	}
	for _, part := range strings.Split(version, ".") {
		if _, err := strconv.Atoi(part); err != nil {
			return errf(CodeInvalidVersion, "version %q is not numeric", version)
		}
	}
	return nil
}

func validateEntrypoint(trust, entrypoint string) error {
	if entrypoint == "" {
		if trust == TrustBuiltin {
			return nil
		}
		return errf(CodeInvalidEntrypoint, "entrypoint is required for trust %q", trust)
	}
	if len(entrypoint) > MaxEntrypointLength {
		return errf(CodeInvalidEntrypoint, "entrypoint exceeds %d characters", MaxEntrypointLength)
	}
	if entrypoint == "." || strings.Contains(entrypoint, "..") || strings.ContainsAny(entrypoint, `/\`) {
		return errf(CodeInvalidEntrypoint, "entrypoint %q must be a bare file name", entrypoint)
	}
	for _, r := range entrypoint {
		if unicode.IsControl(r) || r == ':' {
			return errf(CodeInvalidEntrypoint, "entrypoint %q is not a bare file name", entrypoint)
		}
	}
	return nil
}

// CapabilityRisk validates a capability string and returns its risk class.
func CapabilityRisk(capability string) (Risk, error) {
	if risk, ok := fixedCapabilities[capability]; ok {
		return risk, nil
	}
	if capability == CapNetworkConnect {
		return 0, errf(CodeUnknownCapability, "network.connect requires a concrete domain or *.domain")
	}
	if strings.HasPrefix(capability, CapNetworkConnect+"(") && strings.HasSuffix(capability, ")") {
		domain := capability[len(CapNetworkConnect)+1 : len(capability)-1]
		if validNetworkDomain(domain) {
			return RiskMutate, nil
		}
		return 0, errf(CodeUnknownCapability, "network.connect domain %q is invalid", domain)
	}
	return 0, errf(CodeUnknownCapability, "unknown capability %q", capability)
}

var fixedCapabilities = map[string]Risk{
	CapWorkspaceRead:     RiskObserve,
	CapSessionObserve:    RiskObserve,
	CapAgentSend:         RiskMutate,
	CapAgentApprove:      RiskSensitive,
	CapFilesystemRead:    RiskMutate,
	CapFilesystemWrite:   RiskMutate,
	CapGitWrite:          RiskMutate,
	CapScreenView:        RiskSensitive,
	CapScreenControl:     RiskSensitive,
	CapScreenAudio:       RiskSensitive,
	CapNotificationsSend: RiskMutate,
}

func validNetworkDomain(domain string) bool {
	if domain == "" || len(domain) > 253 || strings.ContainsAny(domain, `/\:@ `) {
		return false
	}
	host := domain
	if strings.HasPrefix(host, "*.") {
		host = host[2:]
	} else if strings.Contains(host, "*") {
		return false
	}
	labels := strings.Split(host, ".")
	if len(labels) < 2 {
		return false
	}
	for _, label := range labels {
		if len(label) == 0 || len(label) > 63 || !domainLabelPattern.MatchString(label) {
			return false
		}
	}
	return true
}

func validateCommands(commands []Command, declared map[string]struct{}) (map[string]Command, error) {
	if len(commands) > MaxCommands {
		return nil, errf(CodeLimitExceeded, "commands exceed %d entries", MaxCommands)
	}
	out := make(map[string]Command, len(commands))
	for _, command := range commands {
		if err := validateContributionID("command", command.ID); err != nil {
			return nil, err
		}
		if _, duplicate := out[command.ID]; duplicate {
			return nil, errf(CodeDuplicateID, "duplicate command id %q", command.ID)
		}
		if err := validateTextField("command title", command.Title, MaxTitleLength, true); err != nil {
			return nil, err
		}
		if err := validateTextField("command description", command.Description, MaxDescriptionLength, false); err != nil {
			return nil, err
		}
		switch command.Scope {
		case CommandScopeSession, CommandScopeWorkspace, CommandScopeGlobal:
		default:
			return nil, errf(CodeUnknownScope, "command %q has unknown scope %q", command.ID, command.Scope)
		}
		for _, required := range command.Requires {
			if _, err := CapabilityRisk(required); err != nil {
				return nil, err
			}
			if _, ok := declared[required]; !ok {
				return nil, errf(CodeUndeclaredCapability, "command %q requires undeclared capability %q", command.ID, required)
			}
		}
		out[command.ID] = command
	}
	return out, nil
}

func validateViews(views []View, commands map[string]Command) (map[string]View, error) {
	if len(views) > MaxViews {
		return nil, errf(CodeLimitExceeded, "views exceed %d entries", MaxViews)
	}
	out := make(map[string]View, len(views))
	for i := range views {
		view := &views[i]
		if err := validateContributionID("view", view.ID); err != nil {
			return nil, err
		}
		if _, duplicate := out[view.ID]; duplicate {
			return nil, errf(CodeDuplicateID, "duplicate view id %q", view.ID)
		}
		switch view.Slot {
		case SlotSessionAction, SlotSessionTab, SlotSessionOverlay, SlotSessionContextPanel, SlotSettingsSection:
		default:
			return nil, errf(CodeUnknownSlot, "view %q has unknown slot %q", view.ID, view.Slot)
		}
		if err := validateTextField("view title", view.Title, MaxTitleLength, true); err != nil {
			return nil, err
		}
		if len(view.Platforms) == 0 {
			view.Platforms = []string{PlatformDesktop}
		}
		for _, platform := range view.Platforms {
			switch platform {
			case PlatformDesktop, PlatformIOS:
			default:
				return nil, errf(CodeUnknownPlatform, "view %q has unknown platform %q", view.ID, platform)
			}
		}
		if view.Command != "" {
			if _, ok := commands[view.Command]; !ok {
				return nil, errf(CodeUnknownCommand, "view %q binds undeclared command %q", view.ID, view.Command)
			}
		}
		if view.Slot == SlotSessionOverlay {
			if view.Overlay == nil {
				return nil, errf(CodeInvalidOverlay, "view %q in slot %s requires an overlay descriptor", view.ID, SlotSessionOverlay)
			}
			if err := validateOverlay(view.Overlay); err != nil {
				return nil, err
			}
		} else if view.Overlay != nil {
			return nil, errf(CodeInvalidOverlay, "view %q in slot %s must not declare an overlay", view.ID, view.Slot)
		}
		out[view.ID] = *view
	}
	return out, nil
}

func validateOverlay(overlay *Overlay) error {
	switch overlay.Anchor {
	case AnchorTopLeft, AnchorTopRight, AnchorBottomLeft, AnchorBottomRight, AnchorCenter:
	default:
		return errf(CodeInvalidOverlay, "overlay anchor %q is unknown", overlay.Anchor)
	}
	if overlay.MaxWidth < MinOverlayPixels || overlay.MaxWidth > MaxOverlayPixels {
		return errf(CodeInvalidOverlay, "overlay max_width must be %d..%d", MinOverlayPixels, MaxOverlayPixels)
	}
	if overlay.MaxHeight < MinOverlayPixels || overlay.MaxHeight > MaxOverlayPixels {
		return errf(CodeInvalidOverlay, "overlay max_height must be %d..%d", MinOverlayPixels, MaxOverlayPixels)
	}
	return nil
}

func validateEvents(events []EventSubscription, declared map[string]struct{}) (map[string]EventSubscription, error) {
	if len(events) > MaxEventSubscriptions {
		return nil, errf(CodeLimitExceeded, "event subscriptions exceed %d entries", MaxEventSubscriptions)
	}
	out := make(map[string]EventSubscription, len(events))
	for _, event := range events {
		if err := validateContributionID("event", event.ID); err != nil {
			return nil, err
		}
		if _, duplicate := out[event.ID]; duplicate {
			return nil, errf(CodeDuplicateID, "duplicate event subscription id %q", event.ID)
		}
		if len(event.Kinds) == 0 {
			return nil, errf(CodeUnknownEventKind, "event subscription %q declares no kinds", event.ID)
		}
		for _, kind := range event.Kinds {
			switch kind {
			case EventWorkspaceChanged, EventSessionUpdated, EventAgentStatus:
			default:
				return nil, errf(CodeUnknownEventKind, "event subscription %q has unknown kind %q", event.ID, kind)
			}
		}
		if _, ok := declared[CapSessionObserve]; !ok {
			return nil, errf(CodeUndeclaredCapability, "event subscription %q requires session.observe to be declared", event.ID)
		}
		out[event.ID] = event
	}
	return out, nil
}

// semver is a numeric major.minor.patch triple.
type semver struct {
	major, minor, patch int
}

func parseSemver(s string) (semver, bool) {
	if !numericVersion.MatchString(s) {
		return semver{}, false
	}
	parts := strings.Split(s, ".")
	values := make([]int, 3)
	for i, part := range parts {
		n, err := strconv.Atoi(part)
		if err != nil {
			return semver{}, false
		}
		values[i] = n
	}
	return semver{major: values[0], minor: values[1], patch: values[2]}, true
}

func compareSemver(a, b semver) int {
	switch {
	case a.major != b.major:
		if a.major < b.major {
			return -1
		}
		return 1
	case a.minor != b.minor:
		if a.minor < b.minor {
			return -1
		}
		return 1
	case a.patch != b.patch:
		if a.patch < b.patch {
			return -1
		}
		return 1
	default:
		return 0
	}
}

type apiConstraint struct {
	op      string
	version semver
}

func parseConstraintVersion(s string) (semver, bool) {
	parts := strings.Split(s, ".")
	if len(parts) != 2 && len(parts) != 3 {
		return semver{}, false
	}
	values := [3]int{}
	for i, part := range parts {
		if part == "" {
			return semver{}, false
		}
		n, err := strconv.Atoi(part)
		if err != nil || n < 0 {
			return semver{}, false
		}
		values[i] = n
	}
	return semver{major: values[0], minor: values[1], patch: values[2]}, true
}

func parseAPIRange(spec string) ([]apiConstraint, error) {
	if strings.TrimSpace(spec) == "" {
		return nil, errf(CodeInvalidAPIRange, "api range is required")
	}
	segments := strings.Split(spec, ",")
	if len(segments) == 0 || len(segments) > MaxAPIRangeBounds {
		return nil, errf(CodeInvalidAPIRange, "api range must contain 1..%d bounds", MaxAPIRangeBounds)
	}
	constraints := make([]apiConstraint, 0, len(segments))
	for _, segment := range segments {
		segment = strings.TrimSpace(segment)
		if segment == "" {
			return nil, errf(CodeInvalidAPIRange, "api range contains an empty bound")
		}
		op := ""
		for _, candidate := range []string{">=", "<=", "==", ">", "<", "="} {
			if strings.HasPrefix(segment, candidate) {
				op = candidate
				segment = strings.TrimSpace(strings.TrimPrefix(segment, candidate))
				break
			}
		}
		if op == "" {
			return nil, errf(CodeInvalidAPIRange, "api bound %q has no comparison operator", segment)
		}
		version, ok := parseConstraintVersion(segment)
		if !ok {
			return nil, errf(CodeInvalidAPIRange, "api bound version %q is malformed", segment)
		}
		constraints = append(constraints, apiConstraint{op: op, version: version})
	}
	return constraints, nil
}

func (c apiConstraint) satisfiedBy(v semver) bool {
	cmp := compareSemver(v, c.version)
	switch c.op {
	case ">=":
		return cmp >= 0
	case "<=":
		return cmp <= 0
	case ">":
		return cmp > 0
	case "<":
		return cmp < 0
	case "=", "==":
		return cmp == 0
	default:
		return false
	}
}

func validateAPIRange(spec string) error {
	constraints, err := parseAPIRange(spec)
	if err != nil {
		return err
	}
	host, _ := parseSemver(APIVersion)
	for _, constraint := range constraints {
		if !constraint.satisfiedBy(host) {
			return errf(CodeInvalidAPIRange, "api range %q does not accept host API %s", spec, APIVersion)
		}
	}
	return nil
}

// readFileLimited reads a file, refusing to allocate more than limit bytes.
func readFileLimited(path string, limit int64) ([]byte, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, errf(CodeInvalidManifest, "read %s: %s", path, err.Error())
	}
	defer f.Close()
	data, err := io.ReadAll(io.LimitReader(f, limit+1))
	if err != nil {
		return nil, errf(CodeInvalidManifest, "read %s: %s", path, err.Error())
	}
	if int64(len(data)) > limit {
		return nil, errf(CodeLimitExceeded, "%s exceeds %d bytes", path, limit)
	}
	return data, nil
}
