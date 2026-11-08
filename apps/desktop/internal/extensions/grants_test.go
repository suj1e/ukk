package extensions

import (
	"os"
	"path/filepath"
	"runtime"
	"testing"
	"time"
)

const grantTestPlugin = "dev.ukk.test.grants"

func TestGrantTTLRules(t *testing.T) {
	tests := []struct {
		name       string
		capability string
		scope      string
		ttl        time.Duration
		wantCode   string
	}{
		{name: "observe long lived", capability: CapWorkspaceRead, scope: ScopeHost, ttl: 30 * 24 * time.Hour},
		{name: "observe without ttl", capability: CapSessionObserve, scope: ScopeHost},
		{name: "mutate without ttl", capability: CapAgentSend, scope: ScopeHost},
		{name: "mutate maximum ttl", capability: CapFilesystemWrite, scope: ScopeHost, ttl: MaxGrantTTL},
		{name: "mutate ttl too long", capability: CapGitWrite, scope: ScopeHost, ttl: MaxGrantTTL + time.Second, wantCode: CodeInvalidGrant},
		{name: "network with domain", capability: "network.connect(api.example.com)", scope: ScopeHost, ttl: time.Hour},
		{name: "sensitive with maximum ttl", capability: CapAgentApprove, scope: ScopeHost, ttl: MaxSensitiveTTL},
		{name: "sensitive without ttl", capability: CapScreenView, scope: ScopeHost, wantCode: CodeInvalidGrant},
		{name: "sensitive ttl too long", capability: CapScreenControl, scope: ScopeHost, ttl: MaxSensitiveTTL + time.Second, wantCode: CodeInvalidGrant},
		{name: "negative ttl", capability: CapWorkspaceRead, scope: ScopeHost, ttl: -time.Second, wantCode: CodeInvalidGrant},
		{name: "bare network capability", capability: CapNetworkConnect, scope: ScopeHost, wantCode: CodeUnknownCapability},
		{name: "invalid scope", capability: CapWorkspaceRead, scope: "space:", wantCode: CodeInvalidScope},
		{name: "unknown scope kind", capability: CapWorkspaceRead, scope: "tenant:abc", wantCode: CodeInvalidScope},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			store, err := OpenGrantStore("")
			if err != nil {
				t.Fatalf("OpenGrantStore() error = %v", err)
			}
			_, err = store.Grant(grantTestPlugin, tc.capability, tc.scope, tc.ttl)
			if tc.wantCode == "" {
				if err != nil {
					t.Fatalf("Grant() error = %v, want success", err)
				}
				return
			}
			if got := CodeOf(err); got != tc.wantCode {
				t.Fatalf("Grant() code = %q (%v), want %q", got, err, tc.wantCode)
			}
		})
	}
}

func TestGrantRejectsBadPlugin(t *testing.T) {
	store, err := OpenGrantStore("")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := store.Grant("Not.Reverse.DNS", CapWorkspaceRead, ScopeHost, 0); !IsCode(err, CodeInvalidID) {
		t.Fatalf("Grant() error = %v, want %s", err, CodeInvalidID)
	}
}

func TestGrantLiveExpiryAndScopeMatching(t *testing.T) {
	store, err := OpenGrantStore("")
	if err != nil {
		t.Fatal(err)
	}
	base := time.Unix(1_700_000_000, 0)
	grant, err := store.GrantAt(grantTestPlugin, CapAgentSend, ScopeHost, 10*time.Minute, base)
	if err != nil {
		t.Fatalf("GrantAt() error = %v", err)
	}
	if grant.ExpiresAtMS != base.Add(10*time.Minute).UnixMilli() {
		t.Fatalf("expires_at_ms = %d, want %d", grant.ExpiresAtMS, base.Add(10*time.Minute).UnixMilli())
	}
	if !store.Authorized(grantTestPlugin, CapAgentSend, ScopeHost, base) {
		t.Fatal("fresh host grant should authorize the host scope")
	}
	if !store.Authorized(grantTestPlugin, CapAgentSend, SessionScope("sess-1"), base) {
		t.Fatal("host grant should cover a session scope")
	}
	if store.Authorized(grantTestPlugin, CapAgentSend, ScopeHost, base.Add(10*time.Minute)) {
		t.Fatal("grant must expire exactly at its deadline")
	}
	if store.Authorized(grantTestPlugin, CapAgentSend, ScopeHost, base.Add(11*time.Minute)) {
		t.Fatal("expired grant still authorized")
	}
	if store.Authorized(grantTestPlugin, CapWorkspaceRead, ScopeHost, base) {
		t.Fatal("grant must not authorize another capability")
	}

	if _, err := store.GrantAt(grantTestPlugin, CapSessionObserve, SessionScope("sess-1"), 0, base); err != nil {
		t.Fatalf("GrantAt(session) error = %v", err)
	}
	if !store.Authorized(grantTestPlugin, CapSessionObserve, SessionScope("sess-1"), base) {
		t.Fatal("session grant should authorize its own session")
	}
	if store.Authorized(grantTestPlugin, CapSessionObserve, SessionScope("sess-2"), base) {
		t.Fatal("session grant leaked into another session")
	}
	if store.Authorized(grantTestPlugin, CapSessionObserve, ScopeHost, base) {
		t.Fatal("session grant must not authorize the host scope")
	}

	if _, err := store.GrantAt(grantTestPlugin, CapWorkspaceRead, SpaceScope("space-1"), 0, base); err != nil {
		t.Fatalf("GrantAt(space) error = %v", err)
	}
	if !store.Authorized(grantTestPlugin, CapWorkspaceRead, SpaceScope("space-1"), base) {
		t.Fatal("space grant should authorize its own space")
	}
	if store.Authorized(grantTestPlugin, CapWorkspaceRead, SessionScope("sess-1"), base) {
		t.Fatal("space grant must not inherit down to a session in v1")
	}
}

func TestGrantPersistenceAndRevocation(t *testing.T) {
	dir := testDir(t)
	path := filepath.Join(dir, "grants.json")
	store, err := OpenGrantStore(path)
	if err != nil {
		t.Fatalf("OpenGrantStore() error = %v", err)
	}
	if _, err := store.Grant(grantTestPlugin, CapSessionObserve, ScopeHost, 0); err != nil {
		t.Fatalf("Grant() error = %v", err)
	}
	info, err := os.Stat(path)
	if err != nil {
		t.Fatalf("stat grant store: %v", err)
	}
	if runtime.GOOS != "windows" && info.Mode().Perm() != 0o600 {
		t.Fatalf("grant store mode = %o, want 600", info.Mode().Perm())
	}

	reopened, err := OpenGrantStore(path)
	if err != nil {
		t.Fatalf("reopen grant store: %v", err)
	}
	if !reopened.Authorized(grantTestPlugin, CapSessionObserve, ScopeHost, time.Now()) {
		t.Fatal("persisted grant should still authorize after reopen")
	}

	revoked, err := reopened.Revoke(grantTestPlugin, CapSessionObserve, ScopeHost)
	if err != nil || !revoked {
		t.Fatalf("Revoke() = %v, %v; want true, nil", revoked, err)
	}
	if reopened.Authorized(grantTestPlugin, CapSessionObserve, ScopeHost, time.Now()) {
		t.Fatal("revoked grant still authorizes")
	}
	after, err := OpenGrantStore(path)
	if err != nil {
		t.Fatalf("reopen after revoke: %v", err)
	}
	if after.Authorized(grantTestPlugin, CapSessionObserve, ScopeHost, time.Now()) {
		t.Fatal("revoked grant resurrected after reopen")
	}
	if after.Len() != 0 {
		t.Fatalf("stored grants = %d, want 0", after.Len())
	}
	again, err := after.Revoke(grantTestPlugin, CapSessionObserve, ScopeHost)
	if err != nil || again {
		t.Fatalf("second Revoke() = %v, %v; want false, nil", again, err)
	}
}

func TestGrantStoreRejectsInsecurePermissions(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("POSIX permission bits are not meaningful on Windows")
	}
	dir := testDir(t)
	path := filepath.Join(dir, "grants.json")
	store, err := OpenGrantStore(path)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := store.Grant(grantTestPlugin, CapWorkspaceRead, ScopeHost, 0); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(path, 0o644); err != nil {
		t.Fatal(err)
	}
	if _, err := OpenGrantStore(path); !IsCode(err, CodeInsecurePermission) {
		t.Fatalf("OpenGrantStore() error = %v, want %s", err, CodeInsecurePermission)
	}
}

func TestGrantStoreRevokeAll(t *testing.T) {
	store, err := OpenGrantStore("")
	if err != nil {
		t.Fatal(err)
	}
	for _, capability := range []string{CapWorkspaceRead, CapSessionObserve} {
		if _, err := store.Grant(grantTestPlugin, capability, ScopeHost, 0); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := store.Grant("dev.ukk.test.other", CapWorkspaceRead, ScopeHost, 0); err != nil {
		t.Fatal(err)
	}
	removed, err := store.RevokeAll(grantTestPlugin)
	if err != nil || removed != 2 {
		t.Fatalf("RevokeAll() = %d, %v; want 2, nil", removed, err)
	}
	if store.Authorized(grantTestPlugin, CapWorkspaceRead, ScopeHost, time.Now()) {
		t.Fatal("RevokeAll left a grant behind")
	}
	if !store.Authorized("dev.ukk.test.other", CapWorkspaceRead, ScopeHost, time.Now()) {
		t.Fatal("RevokeAll removed another plugin's grant")
	}
}

func TestGrantStoreRejectsUnknownVersion(t *testing.T) {
	dir := testDir(t)
	path := filepath.Join(dir, "grants.json")
	if err := os.WriteFile(path, []byte(`{"version":2,"grants":[]}`), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := OpenGrantStore(path); !IsCode(err, CodeInvalidGrant) {
		t.Fatalf("OpenGrantStore() error = %v, want %s", err, CodeInvalidGrant)
	}
}
