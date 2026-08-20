# fetch-or-build.ps1 — the [[build]] step herdr runs on Windows.
#
# The PowerShell counterpart of fetch-or-build.sh: download the prebuilt binary matching this
# checkout's declared version, verify its SHA-256, and install it at
# target\release\herdr-ssh-manager.exe. On ANY miss, explain why and fall back to
# `cargo build --release`, so installing never gets harder than it was without prebuilts.
#
# Note the popup panes do not launch on Windows (herdr resolves the manifest's relative command
# against its own directory) — but the CLI subcommands and `herdr-ssh-manager pick` in a normal
# pane do work, so Windows still needs a built binary.

$ErrorActionPreference = 'Stop'

$repo = 'jorge07RD/herdr-ssh-manager'
$bin  = 'herdr-ssh-manager'

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot  = if ($env:SSHM_REPO_ROOT)  { $env:SSHM_REPO_ROOT }  else { Join-Path $scriptDir '..' }
$cargoToml = if ($env:SSHM_CARGO_TOML) { $env:SSHM_CARGO_TOML } else { Join-Path $repoRoot 'Cargo.toml' }
$out       = if ($env:SSHM_OUT)        { $env:SSHM_OUT }        else { Join-Path $repoRoot 'target\release\herdr-ssh-manager.exe' }
$baseUrl   = if ($env:SSHM_BASE_URL)   { $env:SSHM_BASE_URL }   else { "https://github.com/$repo/releases/download" }

# Put the picker on a key as part of installing, so `herdr plugin install` is the only command
# anyone needs. Safe to do unasked: `setup` is a no-op when the binding already exists and
# refuses a key bound to anything else rather than taking it. Opt out with SSHM_NO_KEYBIND=1,
# and undo it any time with `herdr plugin action invoke unbind-windows`.
function Add-Keybinding {
    if ($env:SSHM_NO_KEYBIND) { return }
    if (-not (Test-Path -LiteralPath $out)) { return }
    # A keybinding that cannot be written must never fail the install; the binary is the
    # part that matters, and `setup` prints its own explanation.
    try { & $out setup } catch { Write-Host "$bin`: could not add the keybinding automatically." }
}

function Build-FromSource {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Write-Error "$bin`: needs Rust 1.88+ to build from source, but cargo was not found. Install Rust from https://rustup.rs and re-run: herdr plugin install $repo"
        exit 1
    }
    Push-Location $repoRoot
    try {
        & cargo build --release
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    } finally { Pop-Location }
    Add-Keybinding
    exit 0
}

function Fallback([string]$reason) {
    Write-Host "$bin`: $reason - building from source instead."
    if ($tmpdir -and (Test-Path $tmpdir)) { Remove-Item -Recurse -Force $tmpdir }
    Build-FromSource
}

# Only x86_64 Windows is published; anything else builds from source.
$arch = $env:PROCESSOR_ARCHITECTURE
if ($arch -ne 'AMD64') { Fallback "no prebuilt binary for Windows/$arch" }
$triple = 'x86_64-pc-windows-msvc'

$version = $null
if (Test-Path $cargoToml) {
    foreach ($line in Get-Content $cargoToml) {
        if ($line -match '^version\s*=\s*"([^"]+)"') { $version = $Matches[1]; break }
    }
}
if (-not $version) { Fallback "could not read the version from $cargoToml" }

$asset  = "$bin-$triple.exe"
$tmpdir = Join-Path ([System.IO.Path]::GetTempPath()) ([System.Guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $tmpdir -Force | Out-Null

$tmpBin  = Join-Path $tmpdir $asset
$tmpSums = Join-Path $tmpdir 'SHA256SUMS'
try {
    Invoke-WebRequest -Uri "$baseUrl/v$version/$asset" -OutFile $tmpBin -UseBasicParsing
} catch { Fallback "no prebuilt binary published for v$version ($asset)" }
try {
    Invoke-WebRequest -Uri "$baseUrl/v$version/SHA256SUMS" -OutFile $tmpSums -UseBasicParsing
} catch { Fallback "no checksums published for v$version" }

# Accept both the text-mode ("  name") and binary-mode (" *name") separators, matching the
# shell script; the release runner may emit either.
$expected = $null
foreach ($line in Get-Content $tmpSums) {
    if ($line -match "^([0-9a-f]{64})\s[\s\*]$([regex]::Escape($asset))$") { $expected = $Matches[1]; break }
}
if (-not $expected) { Fallback "SHA256SUMS lists no checksum for $asset" }

$actual = (Get-FileHash -Algorithm SHA256 -Path $tmpBin).Hash.ToLower()
if ($actual -ne $expected) { Fallback "checksum mismatch for $asset (expected $expected, got $actual)" }

New-Item -ItemType Directory -Path (Split-Path -Parent $out) -Force | Out-Null
Move-Item -Force $tmpBin $out
Remove-Item -Recurse -Force $tmpdir
Write-Host "$bin`: installed prebuilt v$version ($triple), SHA-256 verified."
Add-Keybinding
exit 0
