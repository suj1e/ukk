package extensions

import (
	"bytes"
	"encoding/binary"
	"strings"
	"testing"
)

func TestWireFrameRoundTrip(t *testing.T) {
	var buffer bytes.Buffer
	request := Request{ID: 7, Method: MethodInvoke, Command: "test.echo", Session: "sess-1"}
	if err := WriteFrame(&buffer, request); err != nil {
		t.Fatalf("WriteFrame() error = %v", err)
	}
	payload, err := ReadFrame(&buffer)
	if err != nil {
		t.Fatalf("ReadFrame() error = %v", err)
	}
	var decoded Request
	if err := decodeStrict(payload, &decoded); err != nil {
		t.Fatalf("decode frame: %v", err)
	}
	if decoded.ID != request.ID || decoded.Method != request.Method || decoded.Command != request.Command {
		t.Fatalf("decoded = %+v, want %+v", decoded, request)
	}
}

func TestWireFrameRejectsOversizedAndEmpty(t *testing.T) {
	var buffer bytes.Buffer
	err := WriteFrame(&buffer, strings.Repeat("x", MaxFrameBytes))
	if !IsCode(err, CodeFrameTooLarge) {
		t.Fatalf("WriteFrame(oversized) error = %v, want %s", err, CodeFrameTooLarge)
	}

	var oversized bytes.Buffer
	var header [4]byte
	binary.BigEndian.PutUint32(header[:], uint32(MaxFrameBytes+1))
	oversized.Write(header[:])
	if _, err := ReadFrame(&oversized); !IsCode(err, CodeFrameTooLarge) {
		t.Fatalf("ReadFrame(oversized) error = %v, want %s", err, CodeFrameTooLarge)
	}

	var empty bytes.Buffer
	binary.BigEndian.PutUint32(header[:], 0)
	empty.Write(header[:])
	if _, err := ReadFrame(&empty); !IsCode(err, CodeProtocolViolation) {
		t.Fatalf("ReadFrame(empty) error = %v, want %s", err, CodeProtocolViolation)
	}
}

func TestRedact(t *testing.T) {
	tests := []struct {
		name   string
		input  string
		absent []string
	}{
		{name: "token assignment", input: "token=s3cr3t-value", absent: []string{"s3cr3t-value"}},
		{name: "password colon", input: `password: "hunter2"`, absent: []string{"hunter2"}},
		{name: "api key", input: "api_key=abc123def", absent: []string{"abc123def"}},
		{name: "authorization header", input: "authorization=Bearer abcdefgh", absent: []string{"abcdefgh"}},
		{name: "bearer token", input: "using Bearer abcdefghijklmnop", absent: []string{"abcdefghijklmnop"}},
		{name: "url credentials", input: "https://alice:hunter3@example.com/path", absent: []string{"alice", "hunter3"}},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got := Redact(tc.input)
			for _, absent := range tc.absent {
				if strings.Contains(got, absent) {
					t.Fatalf("Redact(%q) = %q still contains %q", tc.input, got, absent)
				}
			}
		})
	}
}

func TestRedactEscapesControlsAndBounds(t *testing.T) {
	got := Redact("line1\nline2\t\x1b[31mred\x1b[0m \u202eoverride")
	if strings.ContainsAny(got, "\n\t\x1b") {
		t.Fatalf("Redact() kept a control character: %q", got)
	}
	if strings.ContainsRune(got, '\u202e') {
		t.Fatalf("Redact() kept a bidi override: %q", got)
	}
	long := Redact(strings.Repeat("a", 1000))
	if len([]rune(long)) != MaxDetailLength {
		t.Fatalf("redacted length = %d, want %d", len([]rune(long)), MaxDetailLength)
	}
	if !strings.HasSuffix(long, "...") {
		t.Fatalf("truncated text has no marker: %q", long[len(long)-8:])
	}
}

func TestAuditRingIsBoundedAndRedacted(t *testing.T) {
	audit := NewAudit(2)
	audit.Record(AuditRecord{Action: ActionInvoke, Detail: "token=secret-value"})
	audit.Record(AuditRecord{Action: ActionInvoke, Detail: "second"})
	records := audit.Records()
	if len(records) != 2 {
		t.Fatalf("Len() = %d, want 2", len(records))
	}
	if !strings.Contains(records[0].Detail, "[redacted]") {
		t.Fatalf("audit detail was not redacted: %q", records[0].Detail)
	}
	if records[1].Detail != "second" {
		t.Fatalf("records = %+v", records)
	}
	audit.Record(AuditRecord{Action: ActionInvoke, Detail: "third"})
	if audit.Len() != 2 {
		t.Fatalf("Len() = %d, want 2", audit.Len())
	}
	if audit.Dropped() != 1 {
		t.Fatalf("Dropped() = %d, want 1", audit.Dropped())
	}
	records = audit.Records()
	if records[0].Detail != "second" || records[1].Detail != "third" {
		t.Fatalf("Records() = %+v", records)
	}
	for _, record := range records {
		if record.TimeMS == 0 {
			t.Fatalf("record has no timestamp: %+v", record)
		}
	}
}
