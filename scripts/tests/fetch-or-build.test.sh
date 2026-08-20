#!/bin/sh
# Hermetic tests for scripts/fetch-or-build.sh.
#
# No network and no cargo: PATH is pointed at stubs for uname/curl/sha256sum/cargo, and
# SSHM_BASE_URL at a local directory that stands in for the GitHub release. Every fallback
# path matters more than the happy one — a broken fallback means a failed install for
# someone whose platform or network we did not anticipate.
set -u

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
script="$here/../fetch-or-build.sh"
pass=0
fail=0

check() { # check <name> <expected-substring> <actual-output>
    if printf '%s' "$3" | grep -qF "$2"; then
        pass=$((pass + 1))
        echo "  ok   $1"
    else
        fail=$((fail + 1))
        echo "  FAIL $1"
        echo "       expected to find: $2"
        echo "       in: $3"
    fi
}

check_absent() { # check_absent <name> <forbidden-substring> <actual-output>
    if printf '%s' "$3" | grep -qF "$2"; then
        fail=$((fail + 1))
        echo "  FAIL $1"
        echo "       should NOT contain: $2"
        echo "       in: $3"
    else
        pass=$((pass + 1))
        echo "  ok   $1"
    fi
}

# A sandbox with stub binaries, a fake release directory and a fake checkout.
setup() { # setup <uname-machine> [--no-download-tool]
    sandbox=$(mktemp -d)
    mkdir -p "$sandbox/bin" "$sandbox/release/v1.2.3" "$sandbox/repo"
    printf 'version = "1.2.3"\n' > "$sandbox/repo/Cargo.toml"
    # An executable stub rather than opaque bytes: the install script runs the binary it
    # just installed (to add the keybinding), and that call has to be observable.
    cat > "$sandbox/release/v1.2.3/herdr-ssh-manager-x86_64-unknown-linux-musl" <<'EOF'
#!/bin/sh
echo "STUB-BINARY-RAN $*"
EOF

    cat > "$sandbox/bin/uname" <<EOF
#!/bin/sh
[ "\$1" = "-m" ] && echo "$1" && exit 0
echo Linux
EOF
    # curl stub: serves files out of the fake release dir, 404s on anything missing.
    cat > "$sandbox/bin/curl" <<'EOF'
#!/bin/sh
dest=""; url=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) dest="$2"; shift 2 ;;
    -*) shift ;;
    *) url="$1"; shift ;;
  esac
done
src="${url#file://}"
[ -f "$src" ] || exit 22
cp "$src" "$dest"
EOF
    # cargo stub: records that the source build ran instead of actually compiling.
    cat > "$sandbox/bin/cargo" <<EOF
#!/bin/sh
echo "STUB-CARGO-RAN \$*"
touch "$sandbox/cargo-ran"
EOF
    chmod +x "$sandbox/bin/"*

    # A minimal system PATH for the "no download tool" case: everything the script needs,
    # deliberately without curl or wget. Symlinks rather than a stripped PATH, because
    # /bin and /usr/bin carry curl on most machines and would defeat the test.
    mkdir -p "$sandbox/sysbin"
    for tool in sh awk grep sed mktemp head chmod mkdir mv rm cp cat dirname tr; do
        real=$(command -v "$tool" 2>/dev/null) && ln -sf "$real" "$sandbox/sysbin/$tool"
    done
    if [ "${2:-}" = "--no-download-tool" ]; then
        rm -f "$sandbox/bin/curl"
    fi
}

teardown() { rm -rf "$sandbox"; }

run_script() { # run_script [EXTRA=env ...]
    env PATH="$sandbox/bin:/usr/bin:/bin" \
        XDG_CONFIG_HOME="$sandbox/xdg" \
        SSHM_REPO_ROOT="$sandbox/repo" \
        SSHM_CARGO_TOML="$sandbox/repo/Cargo.toml" \
        SSHM_OUT="$sandbox/repo/target/release/herdr-ssh-manager" \
        SSHM_BASE_URL="file://$sandbox/release" \
        HOME="$sandbox" \
        "$@" \
        sh "$script" 2>&1
}

sums_for() { # write a SHA256SUMS naming the asset with digest $1
    printf '%s  herdr-ssh-manager-x86_64-unknown-linux-musl\n' "$1" \
        > "$sandbox/release/v1.2.3/SHA256SUMS"
}
real_digest() {
    sha256sum "$sandbox/release/v1.2.3/herdr-ssh-manager-x86_64-unknown-linux-musl" | awk '{print $1}'
}

echo "fetch-or-build.sh"

# 1. Happy path: correct checksum installs the prebuilt and never calls cargo.
setup x86_64
sums_for "$(real_digest)"
out=$(run_script)
check "installs the verified prebuilt" "SHA-256 verified" "$out"
check_absent "does not build from source" "STUB-CARGO-RAN" "$out"
check "installing also binds the key" "STUB-BINARY-RAN setup" "$out"
[ -f "$sandbox/cargo-ran" ] && { echo "  FAIL cargo ran on the happy path"; fail=$((fail + 1)); } \
                           || { echo "  ok   cargo did not run"; pass=$((pass + 1)); }
if [ -x "$sandbox/repo/target/release/herdr-ssh-manager" ]; then
    echo "  ok   binary installed and executable"; pass=$((pass + 1))
else
    echo "  FAIL binary missing or not executable"; fail=$((fail + 1))
fi
teardown

# 2. A tampered binary must never be installed.
setup x86_64
sums_for "0000000000000000000000000000000000000000000000000000000000000000"
out=$(run_script)
check "checksum mismatch falls back" "checksum mismatch" "$out"
check "  and builds from source" "STUB-CARGO-RAN" "$out"
teardown

# 3. No release published for this version.
setup x86_64
rm -f "$sandbox/release/v1.2.3/herdr-ssh-manager-x86_64-unknown-linux-musl"
out=$(run_script)
check "missing asset falls back" "no prebuilt binary published" "$out"
check "  and builds from source" "STUB-CARGO-RAN" "$out"
teardown

# 4. Binary present but SHA256SUMS absent.
setup x86_64
out=$(run_script)
check "missing checksums falls back" "no checksums published" "$out"
teardown

# 5. SHA256SUMS exists but does not name our asset.
setup x86_64
printf '%s  some-other-file\n' "$(real_digest)" > "$sandbox/release/v1.2.3/SHA256SUMS"
out=$(run_script)
check "unlisted asset falls back" "lists no checksum" "$out"
teardown

# 6. A platform with no published binary (Linux ARM).
setup aarch64
sums_for "$(real_digest)"
out=$(run_script)
check "unmapped platform falls back" "no prebuilt binary for Linux/aarch64" "$out"
check "  and builds from source" "STUB-CARGO-RAN" "$out"
teardown

# 7. Neither curl nor wget available.
setup x86_64 --no-download-tool
sums_for "$(real_digest)"
out=$(env PATH="$sandbox/bin:$sandbox/sysbin" SSHM_REPO_ROOT="$sandbox/repo" \
    SSHM_CARGO_TOML="$sandbox/repo/Cargo.toml" \
    SSHM_OUT="$sandbox/repo/target/release/herdr-ssh-manager" \
    SSHM_BASE_URL="file://$sandbox/release" HOME="$sandbox" \
    "$sandbox/sysbin/sh" "$script" 2>&1)
check "no download tool falls back" "no prebuilt binary published" "$out"
check "  and builds from source" "STUB-CARGO-RAN" "$out"
teardown

# 8. Binary-mode SHA256SUMS (" *name") must verify too.
setup x86_64
printf '%s *herdr-ssh-manager-x86_64-unknown-linux-musl\n' "$(real_digest)" \
    > "$sandbox/release/v1.2.3/SHA256SUMS"
out=$(run_script)
check "binary-mode checksum line verifies" "SHA-256 verified" "$out"
teardown

# 9. The keybinding is a courtesy, not a requirement: it must be refusable.
setup x86_64
sums_for "$(real_digest)"
out=$(run_script SSHM_NO_KEYBIND=1)
check "still installs with the keybinding opted out" "SHA-256 verified" "$out"
check_absent "  and does not touch the keybinding" "STUB-BINARY-RAN setup" "$out"
teardown

# 10. A failed keybinding must never fail the install — the binary is what matters.
setup x86_64
cat > "$sandbox/release/v1.2.3/herdr-ssh-manager-x86_64-unknown-linux-musl" <<'EOF'
#!/bin/sh
echo "STUB-BINARY-RAN $*" >&2
exit 1
EOF
sums_for "$(real_digest)"
out=$(run_script); status=$?
check "reports the install even when binding fails" "SHA-256 verified" "$out"
if [ "$status" -eq 0 ]; then
    echo "  ok   install still succeeds"; pass=$((pass + 1))
else
    echo "  FAIL install failed because of the keybinding (exit $status)"; fail=$((fail + 1))
fi
teardown

echo
echo "passed: $pass  failed: $fail"
[ "$fail" -eq 0 ]
