package agents

import (
	"regexp"
	"strings"
	"unicode"
	"unicode/utf8"
)

// Redaction is conservative on purpose: it rewrites provider-style credentials
// and obvious key=value secrets, and leaves ordinary text alone. Every string
// that originates in the child process (errors, stderr, diagnostics) passes
// through this file before it is stored, logged, or returned.

const redactedPlaceholder = "<redacted>"

var (
	// Provider token prefixes: OpenAI/Anthropic style sk-, pk-, rk-, and
	// GitHub, Slack, Google, and npm credentials.
	reProviderToken = regexp.MustCompile(`(?i)\b(?:sk|pk|rk)-[A-Za-z0-9_\-]{16,}`)
	reGitHubToken   = regexp.MustCompile(`\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})`)
	reSlackToken    = regexp.MustCompile(`\bxox[abprs]-[A-Za-z0-9\-]{10,}`)
	reGoogleKey     = regexp.MustCompile(`\bAIza[A-Za-z0-9_\-]{30,}`)
	reNPMToken      = regexp.MustCompile(`\bnpm_[A-Za-z0-9]{30,}`)
	reAWSKeyID      = regexp.MustCompile(`\b(?:AKIA|ASIA)[A-Z0-9]{16}\b`)
	reJWT           = regexp.MustCompile(`\beyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}`)
	reBearer        = regexp.MustCompile(`(?i)\bBearer\s+[A-Za-z0-9._~+/=\-]{8,}`)
	reAssignment    = regexp.MustCompile(`(?i)\b([A-Za-z0-9_.\-]*(?:key|token|secret|password|credential|passwd)[A-Za-z0-9_.\-]*)(\s*[:=]\s*)(["']?)([A-Za-z0-9._~+/=\-]{8,})(["']?)`)
)

// Redact replaces credential-shaped substrings and strips terminal escape
// sequences. It is safe to call on arbitrary provider text.
func Redact(input string) string {
	if input == "" {
		return ""
	}
	out := stripANSI(input)
	out = reProviderToken.ReplaceAllStringFunc(out, func(token string) string {
		return token[:3] + redactedPlaceholder
	})
	out = reGitHubToken.ReplaceAllString(out, "gh_"+redactedPlaceholder)
	out = reSlackToken.ReplaceAllString(out, "xox-"+redactedPlaceholder)
	out = reGoogleKey.ReplaceAllString(out, "AIza"+redactedPlaceholder)
	out = reNPMToken.ReplaceAllString(out, "npm_"+redactedPlaceholder)
	out = reAWSKeyID.ReplaceAllString(out, "AKIA"+redactedPlaceholder)
	out = reJWT.ReplaceAllString(out, "eyJ"+redactedPlaceholder)
	out = reBearer.ReplaceAllString(out, "Bearer "+redactedPlaceholder)
	out = reAssignment.ReplaceAllStringFunc(out, redactAssignment)
	return out
}

func redactAssignment(match string) string {
	groups := reAssignment.FindStringSubmatch(match)
	if len(groups) != 6 {
		return match
	}
	// A URL such as `https://host/key` must not look like a secret assignment.
	if groups[2] == ":" && strings.HasPrefix(strings.TrimSpace(groups[4]), "//") {
		return match
	}
	return groups[1] + groups[2] + groups[3] + redactedPlaceholder + groups[5]
}

// stripANSI removes CSI and OSC escape sequences. omp writes JSON on stdout, so
// escapes only ever appear as transport noise or in stderr diagnostics.
func stripANSI(input string) string {
	var out strings.Builder
	out.Grow(len(input))
	for i := 0; i < len(input); {
		if input[i] != 0x1b {
			out.WriteByte(input[i])
			i++
			continue
		}
		if i+1 < len(input) && input[i+1] == '[' {
			i += 2
			for i < len(input) {
				b := input[i]
				i++
				if b >= 0x40 && b <= 0x7e {
					break
				}
			}
			continue
		}
		if i+1 < len(input) && input[i+1] == ']' {
			i += 2
			for i < len(input) {
				if input[i] == 0x07 {
					i++
					break
				}
				if input[i] == 0x1b && i+1 < len(input) && input[i+1] == '\\' {
					i += 2
					break
				}
				i++
			}
			continue
		}
		i++
	}
	return out.String()
}

// boundedMessage redacts, drops control characters, and truncates on a UTF-8
// boundary. It is the only way provider text becomes an error or diagnostic.
func boundedMessage(input string, maxBytes int) string {
	if maxBytes <= 0 {
		maxBytes = 2048
	}
	redacted := Redact(input)
	var out strings.Builder
	out.Grow(min(len(redacted), maxBytes))
	for _, r := range redacted {
		if r != '\n' && r != '\t' && unicode.IsControl(r) {
			continue
		}
		if out.Len()+utf8.RuneLen(r) > maxBytes {
			out.WriteString("… [truncated]")
			break
		}
		out.WriteRune(r)
	}
	return out.String()
}

// boundedLine trims one stderr line and bounds it for the diagnostic ring.
func boundedLine(line string, maxBytes int) string {
	line = strings.TrimSpace(line)
	if line == "" {
		return ""
	}
	return boundedMessage(line, maxBytes)
}
