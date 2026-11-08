package extensions

import (
	"fmt"
	"regexp"
	"strings"
	"unicode"
)

// MaxDetailLength is the hard cap for any audit detail after redaction.
const MaxDetailLength = 200

var (
	// key=value / key: value patterns for token-like names. The value may be
	// quoted or bare.
	redactAssignment = regexp.MustCompile(`(?i)\b(authorization|proxy-authorization|token|password|passwd|secret|api[_-]?key|access[_-]?key|private[_-]?key|client[_-]?secret|key|bearer)\b\s*[:=]\s*("(?:[^"\\]|\\.)*"|'[^']*'|[^\s,;]+)`)
	// Bare bearer credentials.
	redactBearer = regexp.MustCompile(`(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{8,}`)
	// Credentials embedded in a URL authority.
	redactURLUser = regexp.MustCompile(`(?i)\b([a-z][a-z0-9+.-]*://)[^/\s:@]+:[^/\s@]+@`)
)

// Redact makes untrusted text safe for a bounded audit record: credential-like
// assignments are elided, control characters (newline, tab, ESC/ANSI) and bidi
// overrides are escaped so they cannot forge log lines, and the result is
// capped at MaxDetailLength characters. It never receives payload bytes.
func Redact(text string) string {
	text = redactBearer.ReplaceAllString(text, "bearer [redacted]")
	text = redactURLUser.ReplaceAllString(text, "${1}[redacted]@")
	text = redactAssignment.ReplaceAllString(text, "${1}=[redacted]")
	text = escapeControls(text)
	return truncateRunes(text, MaxDetailLength)
}

func escapeControls(text string) string {
	var b strings.Builder
	b.Grow(len(text))
	for _, r := range text {
		switch {
		case r == '\n':
			b.WriteString(`\n`)
		case r == '\r':
			b.WriteString(`\r`)
		case r == '\t':
			b.WriteString(`\t`)
		case unicode.IsControl(r) || isBidiControl(r):
			if r <= 0xFF {
				fmt.Fprintf(&b, `\x%02x`, r)
			} else {
				fmt.Fprintf(&b, `\u%04x`, r)
			}
		default:
			b.WriteRune(r)
		}
	}
	return b.String()
}

func isBidiControl(r rune) bool {
	switch {
	case r == 0x200E || r == 0x200F:
		return true
	case r >= 0x202A && r <= 0x202E:
		return true
	case r >= 0x2066 && r <= 0x2069:
		return true
	default:
		return false
	}
}

func truncateRunes(text string, max int) string {
	if max < 4 {
		max = 4
	}
	runes := []rune(text)
	if len(runes) <= max {
		return text
	}
	return string(runes[:max-3]) + "..."
}
