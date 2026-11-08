// Package workspace owns the durable product state: Spaces, Sessions,
// AgentRuns, and per-Session pane layouts. It never starts, attaches, or
// signals a terminal or Agent.
package workspace

import (
	"crypto/rand"
	"encoding/hex"
	"errors"
	"strings"
	"time"
)

// CurrentRevision is the schema revision of the persisted document.
const CurrentRevision = 1

var (
	// ErrNotFound is returned when an ID does not name a stored record.
	ErrNotFound = errors.New("workspace: not found")
	// ErrInvalid is returned for a record that fails validation.
	ErrInvalid = errors.New("workspace: invalid")
	// ErrConflict is returned when an optimistic revision does not match.
	ErrConflict = errors.New("workspace: layout revision conflict")
	// ErrAlreadyBound is returned for a terminal already bound to a Session.
	ErrAlreadyBound = errors.New("workspace: terminal already bound")
)

// Space is a named project root.
type Space struct {
	ID          string `json:"id"`
	Title       string `json:"title"`
	ProjectRoot string `json:"project_root"`
	CreatedMS   int64  `json:"created_ms"`
}

// Session is a human work context that can contain many AgentRuns.
type Session struct {
	ID        string `json:"id"`
	SpaceID   string `json:"space_id"`
	Title     string `json:"title"`
	Lifecycle string `json:"lifecycle"`
	CreatedMS int64  `json:"created_ms"`
	UpdatedMS int64  `json:"updated_ms"`
}

// SessionLifecycle values. Archiving never stops a terminal.
const (
	LifecycleActive   = "active"
	LifecyclePaused   = "paused"
	LifecycleArchived = "archived"
)

// AgentRun binds one Agent process, identified by a herdr-core terminal
// address, to a Session. Its existence is not proof that the process is live.
type AgentRun struct {
	ID              string `json:"id"`
	SessionID       string `json:"session_id"`
	ParentID        string `json:"parent_id,omitempty"`
	CoreID          string `json:"core_id"`
	TerminalAddress string `json:"terminal_address"`
	Adapter         string `json:"adapter"`
	Status          string `json:"status"`
	CreatedMS       int64  `json:"created_ms"`
}

// AgentRun observation states.
const (
	AgentCreated  = "created"
	AgentRunning  = "running"
	AgentBlocked  = "blocked"
	AgentFinished = "finished"
	AgentFailed   = "failed"
	AgentUnknown  = "unknown"
)

// Snapshot is a read-only view of all durable records.
type Snapshot struct {
	Revision int64                    `json:"revision"`
	Spaces   []Space                  `json:"spaces"`
	Sessions []Session                `json:"sessions"`
	Agents   []AgentRun               `json:"agents"`
	Layouts  map[string]SessionLayout `json:"layouts"`
}

// NewID returns a random UUIDv4.
func NewID() string { return newID() }

func newID() string {
	var b [16]byte
	if _, err := rand.Read(b[:]); err != nil {
		panic("workspace: crypto/rand unavailable: " + err.Error())
	}
	b[6] = (b[6] & 0x0f) | 0x40
	b[8] = (b[8] & 0x3f) | 0x80
	dst := make([]byte, 36)
	hex.Encode(dst[0:8], b[0:4])
	dst[8] = '-'
	hex.Encode(dst[9:13], b[4:6])
	dst[13] = '-'
	hex.Encode(dst[14:18], b[6:8])
	dst[18] = '-'
	hex.Encode(dst[19:23], b[8:10])
	dst[23] = '-'
	hex.Encode(dst[24:36], b[10:16])
	return string(dst)
}

func nowMS() int64 { return time.Now().UnixMilli() }

func cleanTitle(raw string) (string, error) {
	title := strings.TrimSpace(raw)
	if title == "" || len([]rune(title)) > 200 {
		return "", ErrInvalid
	}
	return title, nil
}

func cleanRoot(raw string) (string, error) {
	root := strings.TrimSpace(raw)
	if root == "" || len([]rune(root)) > 4096 {
		return "", ErrInvalid
	}
	return root, nil
}
