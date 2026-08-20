# launch.ps1 — run the plugin binary by absolute path. Windows only.
#
# herdr hands a pane/action's relative program straight to CreateProcessW, which resolves
# it against herdr's OWN directory rather than the plugin root, and never appends `.exe`.
# So `./target/release/herdr-ssh-manager` simply cannot spawn on Windows — the popup never
# opens and the keybinding looks dead.
#
# The Windows manifest entries therefore run `powershell` (which IS on PATH, so
# CreateProcessW finds it) and point it at this script by absolute path. From here on
# every path is absolute, so nothing depends on the process cwd.

[CmdletBinding()]
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]] $Rest
)

$ErrorActionPreference = 'Stop'

if ($null -eq $Rest) { $Rest = @() }

# $PSScriptRoot is where this file lives, so the plugin root is its parent. That beats both
# the cwd (unreliable here) and a round-trip to `herdr plugin list`.
$root = Split-Path -Parent $PSScriptRoot
if ($root.StartsWith('\\?\')) { $root = $root.Substring(4) }

$exe = Join-Path $root 'target\release\herdr-ssh-manager.exe'
if (-not (Test-Path -LiteralPath $exe)) {
    Write-Error "herdr-ssh-manager.exe not found at $exe -- reinstall the plugin so its build step runs."
    exit 1
}

& $exe @Rest
exit $LASTEXITCODE
