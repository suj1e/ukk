set shell := ["bash", "-uc"]

default: check

# Fetch both workspaces without building.
fetch:
    cargo fetch
    cargo fetch --manifest-path apps/desktop/Cargo.toml

fmt:
    cargo fmt --all
    cargo fmt --manifest-path apps/desktop/Cargo.toml --all

fmt-check:
    cargo fmt --all -- --check
    cargo fmt --manifest-path apps/desktop/Cargo.toml --all -- --check

test:
    cargo test --workspace

clippy:
    cargo clippy --workspace --all-targets -- -D warnings

docs:
    python3 scripts/check_docs.py
    python3 scripts/check_versions.py

# Prove archive layout, Info.plist, checksum coverage and determinism with
# fake binaries; runs on macOS, Linux and Windows without extra dependencies.
package-test:
    python3 scripts/test_package_release.py

desktop-check:
    cargo fmt --manifest-path apps/desktop/Cargo.toml --all -- --check
    # Local check uses the runtime-shader escape hatch when the Metal Toolchain
    # is absent. CI's desktop workflow builds the default features.
    cargo check --manifest-path apps/desktop/Cargo.toml --features gpui_platform/runtime_shaders

# CI shader escape hatch only; never publish binaries built with this feature.
desktop-test:
    cargo test --manifest-path apps/desktop/Cargo.toml --features gpui_platform/runtime_shaders

check: docs package-test fmt-check test clippy desktop-check

host:
    cargo run -p ukk-host -- serve

desktop:
    cargo run --manifest-path apps/desktop/Cargo.toml --bin ukk-desktop

# Build one unsigned archive for the current OS/arch. Override with
# UKK_BUILD_VERSION, UKK_BUILD_COMMIT, UKK_PACKAGE_ARCH or UKK_PACKAGE_OS.
package:
    #!/usr/bin/env bash
    set -euo pipefail
    version="${UKK_BUILD_VERSION:-0.1.0-dev.local}"
    commit="${UKK_BUILD_COMMIT:-$(git rev-parse HEAD)}"
    arch="${UKK_PACKAGE_ARCH:-$(uname -m)}"
    os="${UKK_PACKAGE_OS:-}"
    if [ -z "$os" ]; then
      case "$(uname -s)" in
        Darwin) os=macos ;;
        Linux) os=linux ;;
        *) echo "unsupported OS; set UKK_PACKAGE_OS" >&2; exit 2 ;;
      esac
    fi
    UKK_BUILD_VERSION="$version" UKK_BUILD_COMMIT="$commit" \
      python3 scripts/package_release.py --os "$os" --arch "$arch" \
      --version "$version" --commit "$commit" --out-dir dist

release: package
