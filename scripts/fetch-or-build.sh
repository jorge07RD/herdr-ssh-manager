#!/bin/sh
# fetch-or-build.sh — the [[build]] step herdr runs on `herdr plugin install`.
#
# Fast path: download the prebuilt binary matching this checkout's declared version and this
# machine's platform, verify its SHA-256, and install it at target/release/herdr-ssh-manager —
# exactly where herdr-plugin.toml's [[panes]] and [[actions]] expect it.
#
# Fallback: on ANY miss — no release for this version, no asset for this platform, download
# failure, checksum mismatch, no curl/wget, no sha256 tool — say why and build from source with
# cargo. Installing therefore never becomes harder than it was before prebuilts existed.
#
# The match is by declared VERSION, not by commit: a checkout ahead of the last tag still uses
# the released binary instead of forcing a compile. Integrity is unaffected — the binary is still
# SHA-256 verified — and a version with no published release simply 404s into the source build,
# so a binary whose version differs from this source can never be installed silently.
#
# SSHM_* env vars override every path and the release URL so the logic can be exercised by a
# hermetic test with stubbed uname/curl/cargo — see scripts/tests/fetch-or-build.test.sh.
set -u

repo="jorge07RD/herdr-ssh-manager"
bin="herdr-ssh-manager"

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root="${SSHM_REPO_ROOT:-$script_dir/..}"
cargo_toml="${SSHM_CARGO_TOML:-$repo_root/Cargo.toml}"
out="${SSHM_OUT:-$repo_root/target/release/$bin}"
base_url="${SSHM_BASE_URL:-https://github.com/$repo/releases/download}"

have() { command -v "$1" >/dev/null 2>&1; }

# Build from source: the original, unconditional behaviour. ~/.cargo/env is sourced first because
# herdr can be launched without ~/.cargo/bin on PATH (a GUI or login-less start), which would
# otherwise make cargo look missing on a machine that has it. The [ -f ] guard keeps a missing
# env file from aborting the build.
build_from_source() {
    [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
    if ! have cargo; then
        echo "$bin: needs Rust 1.88+ to build from source, but cargo was not found." >&2
        echo "$bin: install Rust from https://rustup.rs and re-run: herdr plugin install $repo" >&2
        exit 1
    fi
    cd "$repo_root" || exit 1
    exec cargo build --release
}

fallback() {
    echo "$bin: $1 — building from source instead." >&2
    [ -n "${tmpdir:-}" ] && rm -rf "$tmpdir"
    build_from_source
}

download() { # download <url> <dest>
    if have curl; then
        curl -fsSL -o "$2" "$1"
    elif have wget; then
        wget -q -O "$2" "$1"
    else
        return 127
    fi
}

sha256_of() { # prints the hex digest of file $1
    if have sha256sum; then
        sha256sum "$1" | awk '{print $1}'
    elif have shasum; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        return 127
    fi
}

# --- which prebuilt fits this machine ---------------------------------------------------
os=$(uname -s 2>/dev/null || echo unknown)
arch=$(uname -m 2>/dev/null || echo unknown)
triple=""
case "$os" in
    Darwin)
        case "$arch" in
            arm64 | aarch64) triple="aarch64-apple-darwin" ;;
            x86_64 | amd64) triple="x86_64-apple-darwin" ;;
        esac
        ;;
    Linux)
        # Only x86_64 is published for Linux; ARM (Raspberry Pi, ARM servers) builds from source.
        case "$arch" in
            x86_64 | amd64) triple="x86_64-unknown-linux-musl" ;;
        esac
        ;;
esac
[ -n "$triple" ] || fallback "no prebuilt binary for $os/$arch"

# --- the version this checkout declares -------------------------------------------------
version=$(grep -E '^version *= *"' "$cargo_toml" 2>/dev/null | head -n 1 | sed -E 's/^version *= *"([^"]+)".*/\1/')
[ -n "$version" ] || fallback "could not read the version from $cargo_toml"

asset="$bin-$triple"
tmpdir=$(mktemp -d 2>/dev/null) || fallback "could not create a temp dir"
trap 'rm -rf "$tmpdir"' EXIT

download "$base_url/v$version/$asset" "$tmpdir/$asset" \
    || fallback "no prebuilt binary published for v$version ($asset)"
download "$base_url/v$version/SHA256SUMS" "$tmpdir/SHA256SUMS" \
    || fallback "no checksums published for v$version"

# The expected digest is the SHA256SUMS line naming our asset. coreutils text mode separates
# with two spaces and binary mode with " *", so accept either — a release built on a runner
# using binary mode must still verify rather than silently forcing a source build.
expected=$(grep -E "^[0-9a-f]{64} [ *]$asset\$" "$tmpdir/SHA256SUMS" 2>/dev/null | awk '{print $1}' | head -n 1)
[ -n "$expected" ] || fallback "SHA256SUMS lists no checksum for $asset"

actual=$(sha256_of "$tmpdir/$asset") || fallback "no sha-256 tool (sha256sum or shasum) available"
[ "$actual" = "$expected" ] \
    || fallback "checksum mismatch for $asset (expected $expected, got $actual)"

chmod +x "$tmpdir/$asset"
mkdir -p "$(dirname "$out")"
mv -f "$tmpdir/$asset" "$out" || fallback "could not install the verified binary to $out"
echo "$bin: installed prebuilt v$version ($triple), SHA-256 verified."
exit 0
