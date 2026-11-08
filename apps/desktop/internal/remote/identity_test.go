package remote

import (
	"crypto/ed25519"
	"os"
	"path/filepath"
	"runtime"
	"testing"
)

func TestIdentitySaveLoad(t *testing.T) {
	id, err := GenerateIdentity()
	if err != nil {
		t.Fatalf("generate: %v", err)
	}
	defer id.Zeroize()
	pub := id.Public()
	if pub.Ed.IsZero() || pub.X.IsZero() {
		t.Fatal("public bundle has zero keys")
	}
	msg := []byte("ukk identity test")
	sig := id.Sign(msg)
	if !ed25519.Verify(pub.Ed[:], msg, sig) {
		t.Fatal("signature does not verify")
	}

	path := filepath.Join(t.TempDir(), "device.json")
	if err := SaveIdentity(path, id); err != nil {
		t.Fatalf("save: %v", err)
	}
	info, err := os.Stat(path)
	if err != nil {
		t.Fatalf("stat: %v", err)
	}
	if runtime.GOOS != "windows" && info.Mode().Perm() != 0o600 {
		t.Fatalf("mode = %o, want 0600", info.Mode().Perm())
	}
	loaded, err := LoadIdentity(path)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	defer loaded.Zeroize()
	if loaded.Public() != pub {
		t.Fatal("loaded public keys differ")
	}
	if got := loaded.Fingerprint(); got != id.Fingerprint() {
		t.Fatalf("fingerprint = %q, want %q", got, id.Fingerprint())
	}
	if !ed25519.Verify(pub.Ed[:], msg, loaded.Sign(msg)) {
		t.Fatal("loaded identity cannot sign for the saved key")
	}
}

func TestIdentityRejectsInsecureFile(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("POSIX mode bits are not enforced on Windows")
	}
	id, err := GenerateIdentity()
	if err != nil {
		t.Fatal(err)
	}
	defer id.Zeroize()
	dir := t.TempDir()
	path := filepath.Join(dir, "device.json")
	if err := SaveIdentity(path, id); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(path, 0o644); err != nil {
		t.Fatal(err)
	}
	if _, err := LoadIdentity(path); !HasCode(err, CodeIdentityInvalid) {
		t.Fatalf("LoadIdentity error = %v, want identity_invalid", err)
	}

	link := filepath.Join(dir, "link.json")
	if err := os.Symlink(path, link); err != nil {
		t.Fatal(err)
	}
	if _, err := LoadIdentity(link); !HasCode(err, CodeIdentityInvalid) {
		t.Fatalf("symlink error = %v, want identity_invalid", err)
	}
}

func TestIdentityRejectsMismatchedPublicKeys(t *testing.T) {
	id, err := GenerateIdentity()
	if err != nil {
		t.Fatal(err)
	}
	defer id.Zeroize()
	path := filepath.Join(t.TempDir(), "device.json")
	if err := SaveIdentity(path, id); err != nil {
		t.Fatal(err)
	}
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var file identityFile
	if err := decodeStrict(raw, &file); err != nil {
		t.Fatal(err)
	}
	file.EdPublic = Key{1}
	bad, err := jsonMarshal(file)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, bad, 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := LoadIdentity(path); !HasCode(err, CodeIdentityInvalid) {
		t.Fatalf("error = %v, want identity_invalid", err)
	}
}
