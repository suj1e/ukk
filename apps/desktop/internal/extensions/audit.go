package extensions

import (
	"sync"
	"time"
)

// DefaultAuditCapacity is the retained record count of the supervisor's audit
// ring. The oldest record is evicted and counted when the ring is full.
const DefaultAuditCapacity = 512

// Audit actions.
const (
	ActionInstall   = "install"
	ActionEnable    = "enable"
	ActionDisable   = "disable"
	ActionUninstall = "uninstall"
	ActionInvoke    = "invoke"
	ActionGrant     = "grant"
	ActionRevoke    = "revoke"
)

// Audit outcomes.
const (
	OutcomeSuccess = "success"
	OutcomeDenied  = "denied"
	OutcomeError   = "error"
	OutcomeInfo    = "info"
)

// AuditRecord is one bounded decision record. Detail is always redacted and
// never carries command arguments, event payloads, file content or helper
// stdout.
type AuditRecord struct {
	TimeMS     int64  `json:"time_ms"`
	Plugin     string `json:"plugin,omitempty"`
	Action     string `json:"action"`
	Capability string `json:"capability,omitempty"`
	Scope      string `json:"scope,omitempty"`
	Outcome    string `json:"outcome"`
	Detail     string `json:"detail,omitempty"`
}

// Audit is a bounded in-memory ring of redacted records.
type Audit struct {
	mu       sync.Mutex
	capacity int
	now      func() time.Time
	records  []AuditRecord
	next     int
	dropped  uint64
}

// NewAudit creates a bounded audit ring. A non-positive capacity uses
// DefaultAuditCapacity.
func NewAudit(capacity int) *Audit {
	if capacity <= 0 {
		capacity = DefaultAuditCapacity
	}
	return &Audit{capacity: capacity, now: time.Now}
}

// Record appends one record, redacting its detail and stamping the time when
// absent. It never blocks on I/O and never grows without bound.
func (a *Audit) Record(record AuditRecord) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if record.TimeMS == 0 {
		record.TimeMS = a.now().UnixMilli()
	}
	record.Detail = Redact(record.Detail)
	if len(a.records) < a.capacity {
		a.records = append(a.records, record)
		return
	}
	a.records[a.next] = record
	a.next = (a.next + 1) % a.capacity
	a.dropped++
}

// Records returns a copy of the retained records, oldest first.
func (a *Audit) Records() []AuditRecord {
	a.mu.Lock()
	defer a.mu.Unlock()
	out := make([]AuditRecord, 0, len(a.records))
	if len(a.records) < a.capacity {
		return append(out, a.records...)
	}
	out = append(out, a.records[a.next:]...)
	out = append(out, a.records[:a.next]...)
	return out
}

// Len returns the number of retained records.
func (a *Audit) Len() int {
	a.mu.Lock()
	defer a.mu.Unlock()
	return len(a.records)
}

// Dropped returns the number of records evicted because the ring was full.
func (a *Audit) Dropped() uint64 {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.dropped
}
