set shell := ["bash", "-uc"]

default: check

# Repository layout and Go module metadata.
layout:
    python3 scripts/check_layout.py

# Required documentation, relative links and AGENTS.md length.
docs:
    python3 scripts/check_docs.py

# mygo.json contract and iOS MARKETING_VERSION agreement.
versions:
    python3 scripts/check_versions.py

fmt-check:
    test -z "$(cd apps/desktop && gofmt -l .)"

build:
    cd apps/desktop && GOTOOLCHAIN=go1.27.3 go build ./...

test:
    cd apps/desktop && GOTOOLCHAIN=go1.27.3 go test ./...

race:
    cd apps/desktop && GOTOOLCHAIN=go1.27.3 go test -race ./internal/hc ./internal/agents ./internal/remote ./internal/relay ./internal/extensions ./internal/workspace ./internal/bridge

# Launch the native desktop against this machine's herdr-core.
app:
    cd apps/desktop && GOTOOLCHAIN=go1.27.3 go run ./cmd/ukk

# Run the background bridge only (agents, pairing, Remote Screen).
bridge:
    cd apps/desktop && GOTOOLCHAIN=go1.27.3 go run ./cmd/ukk-bridge serve

# Unsigned package for the current OS (MyGo downloads its native terminal
# library and, on Windows, NSIS unless installed).
package:
    cd apps/desktop && GOTOOLCHAIN=go1.27.3 go tool mygo build

# iOS project regeneration and simulator tests (macOS only).
ios-project:
    cd apps/ios && xcodegen generate

check: layout docs versions fmt-check build test
