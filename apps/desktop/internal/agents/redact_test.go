package agents

import (
	"strings"
	"testing"
)

func TestRedactProviderTokens(t *testing.T) {
	token := "sk-" + strings.Repeat("a1B2", 8)
	text := "request failed: invalid key " + token + " token"
	redacted := Redact(text)
	if strings.Contains(redacted, token) {
		t.Fatalf("token survived: %s", redacted)
	}
	if !strings.Contains(redacted, "sk-<redacted>") {
		t.Fatalf("redaction marker missing: %s", redacted)
	}
	if !strings.Contains(redacted, "request failed") {
		t.Fatalf("ordinary text lost: %s", redacted)
	}
}

func TestRedactBearerAndAssignments(t *testing.T) {
	text := `Authorization: Bearer abcdef1234567890 api_key="super-secret-value" password: hunter2hunter2`
	redacted := Redact(text)
	for _, secret := range []string{"abcdef1234567890", "super-secret-value", "hunter2hunter2"} {
		if strings.Contains(redacted, secret) {
			t.Fatalf("%q survived in %s", secret, redacted)
		}
	}
	if !strings.Contains(redacted, "api_key=") {
		t.Fatalf("key name lost: %s", redacted)
	}
}

func TestRedactOrdinaryText(t *testing.T) {
	text := "Bash failed: exit code 1. key=value models like gpt-5 are fine."
	if got := Redact(text); got != text {
		t.Fatalf("ordinary text rewritten: %q", got)
	}
	// A URL path is not a secret assignment.
	url := "fetch https://example.test/key/abcdefgh"
	if got := Redact(url); got != url {
		t.Fatalf("url rewritten: %q", got)
	}
}

func TestRedactANSIAndBounding(t *testing.T) {
	if got := Redact("\x1b[31mred\x1b[0m text"); got != "red text" {
		t.Fatalf("ansi = %q", got)
	}
	bounded := boundedMessage(strings.Repeat("é", 100), 16)
	if len(bounded) > 16+len("… [truncated]") {
		t.Fatalf("bounded too long: %q", bounded)
	}
	if !strings.HasSuffix(bounded, "[truncated]") {
		t.Fatalf("bounded not marked: %q", bounded)
	}
	if got := boundedMessage("a\x00b\nc", 64); got != "ab\nc" {
		t.Fatalf("control characters = %q", got)
	}
}
