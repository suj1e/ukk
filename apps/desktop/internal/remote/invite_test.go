package remote

import (
	"encoding/json"
	"fmt"
	"strings"
	"testing"
	"time"
)

func mustIdentity(t *testing.T) *Identity {
	t.Helper()
	id, err := GenerateIdentity()
	if err != nil {
		t.Fatalf("generate identity: %v", err)
	}
	t.Cleanup(id.Zeroize)
	return id
}

func mustInvite(t *testing.T, now time.Time) (Invite, *Identity) {
	t.Helper()
	id := mustIdentity(t)
	invite, err := newInviteAt("host-1", id, "wss://relay.example/ws", 2*time.Minute, now)
	if err != nil {
		t.Fatalf("new invite: %v", err)
	}
	return invite, id
}

func jsonMarshal(v any) ([]byte, error) { return json.Marshal(v) }

func TestInviteRoundTrip(t *testing.T) {
	now := time.UnixMilli(1_762_550_000_000)
	invite, id := mustInvite(t, now)
	if invite.V != 1 || invite.HostID != "host-1" {
		t.Fatalf("unexpected invite: %+v", invite)
	}
	if invite.HostEd != id.Public().Ed || invite.HostX != id.Public().X {
		t.Fatal("invite keys do not match identity")
	}
	raw, err := invite.Marshal()
	if err != nil {
		t.Fatal(err)
	}
	parsed, err := ParseInvite(raw, now)
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if parsed.Secret != invite.Secret {
		t.Fatal("secret not round-tripped")
	}
	if parsed.ExpireMS != invite.ExpireMS {
		t.Fatal("expiry not round-tripped")
	}

	// base64url keys are unpadded and 43 characters for 32 bytes.
	var wire map[string]any
	if err := json.Unmarshal(raw, &wire); err != nil {
		t.Fatal(err)
	}
	ed, _ := wire["host_ed"].(string)
	if len(ed) != 43 || strings.ContainsAny(ed, "+/=") {
		t.Fatalf("host_ed %q is not unpadded base64url", ed)
	}
	if strings.Contains(fmt.Sprint(invite), invite.Secret.String()) {
		t.Fatal("invite formatting leaked a redaction marker as data")
	}
	if strings.Contains(fmt.Sprintf("%v", invite), encodeBase64URL(invite.Secret[:])) {
		t.Fatal("invite formatting leaked the secret")
	}
}

func TestInviteRejectsInvalidForms(t *testing.T) {
	now := time.UnixMilli(1_762_550_000_000)
	invite, _ := mustInvite(t, now)
	raw, err := invite.Marshal()
	if err != nil {
		t.Fatal(err)
	}

	withExtra := append(append([]byte{}, raw[:len(raw)-1]...), []byte(`,"extra":1}`)...)
	if _, err := ParseInvite(withExtra, now); !HasCode(err, CodeInviteInvalid) {
		t.Fatalf("unknown field error = %v", err)
	}
	trailing := append(append([]byte{}, raw...), []byte(`{}`)...)
	if _, err := ParseInvite(trailing, now); !HasCode(err, CodeInviteInvalid) {
		t.Fatalf("trailing data error = %v", err)
	}

	badVersion := invite
	badVersion.V = 2
	if err := badVersion.Validate(now); !HasCode(err, CodeInviteInvalid) {
		t.Fatalf("version error = %v", err)
	}
	longExpiry := invite
	longExpiry.ExpireMS = now.Add(MaxInviteTTL + time.Millisecond).UnixMilli()
	if err := longExpiry.Validate(now); !HasCode(err, CodeInviteInvalid) {
		t.Fatalf("long expiry error = %v", err)
	}
	expired := invite
	expired.ExpireMS = now.UnixMilli()
	if err := expired.Validate(now); !HasCode(err, CodeInviteExpired) {
		t.Fatalf("expired error = %v", err)
	}
	for _, relay := range []string{"ws://relay.example", "https://relay.example", "relay.example", "wss://user@relay.example"} {
		badRelay := invite
		badRelay.Relay = relay
		if err := badRelay.Validate(now); !HasCode(err, CodeInviteInvalid) {
			t.Fatalf("relay %q error = %v", relay, err)
		}
	}
}

func TestInviteSecretIsRedactedInErrors(t *testing.T) {
	secretText := encodeBase64URL(make([]byte, 32))
	raw := []byte(`{"v":1,"host_id":"host-1","host_ed":"` + Key{1}.String() +
		`","host_x":"` + Key{2}.String() + `","invite_id":"invite-1","secret":"` + secretText +
		`x","expire_ms":1,"relay":"wss://relay.example"}`)
	_, err := ParseInvite(raw, time.Now())
	if err == nil {
		t.Fatal("expected error")
	}
	if strings.Contains(err.Error(), secretText) || strings.Contains(err.Error(), "secret\":") {
		t.Fatalf("error leaked invite payload: %v", err)
	}
}

func TestInviteStoreSingleUse(t *testing.T) {
	now := time.UnixMilli(1_762_550_000_000)
	invite, _ := mustInvite(t, now)
	store := NewInviteStore()
	store.now = func() time.Time { return now }
	if err := store.Add(invite); err != nil {
		t.Fatalf("add: %v", err)
	}
	if err := store.Add(invite); !HasCode(err, CodeInviteInvalid) {
		t.Fatalf("duplicate add error = %v", err)
	}

	claimed, err := store.Begin(invite.InviteID)
	if err != nil {
		t.Fatalf("begin: %v", err)
	}
	if claimed.Secret != invite.Secret {
		t.Fatal("begin returned wrong secret")
	}
	if _, err := store.Begin(invite.InviteID); !HasCode(err, CodeInviteInProgress) {
		t.Fatalf("second begin error = %v", err)
	}
	if err := store.Complete(invite.InviteID); err != nil {
		t.Fatalf("complete: %v", err)
	}
	if _, err := store.Begin(invite.InviteID); !HasCode(err, CodeInviteUsed) {
		t.Fatalf("after complete error = %v", err)
	}
	if _, err := store.Begin("missing"); !HasCode(err, CodeInviteUnknown) {
		t.Fatalf("unknown error = %v", err)
	}

	expiredInvite := invite
	expiredInvite.InviteID = "expired-1"
	expiredInvite.ExpireMS = now.Add(time.Second).UnixMilli()
	if err := store.Add(expiredInvite); err != nil {
		t.Fatal(err)
	}
	store.now = func() time.Time { return now.Add(2 * time.Second) }
	if _, err := store.Begin(expiredInvite.InviteID); !HasCode(err, CodeInviteExpired) {
		t.Fatalf("expired begin error = %v", err)
	}
}

func TestSecretZeroize(t *testing.T) {
	var secret Secret
	for i := range secret {
		secret[i] = byte(i + 1)
	}
	secret.Zeroize()
	var zero Secret
	if secret != zero {
		t.Fatal("secret was not zeroized")
	}
	if got := secret.String(); got != "[redacted]" {
		t.Fatalf("String = %q", got)
	}
}
