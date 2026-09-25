# kvmshare bootstrap installer (Windows PowerShell 5.1+ / PowerShell 7).
#
#   irm https://github.com/YELrhilassi/kvmshare/releases/latest/download/install.ps1 | iex
#   .\install.ps1                  # latest
#   .\install.ps1 -Tag v0.8.7      # pinned version
#   $env:KVMSHARE_UPSTREAM='owner/repo'; .\install.ps1
#
# This script does NOT implement installation. It gets the real installer
# - the Go kvmshare-install binary published with every release - onto
# the machine verified, then hands off to it (it downloads the platform
# archive, verifies it, applies binaries atomically, and performs the
# Windows desktop integration: shortcuts, elevation tasks, firewall
# rules, Add/Remove Programs).
#
# Why a script channel at all: Windows SmartScreen blocks unsigned exes
# *downloaded through a browser* because those files carry
# Mark-of-the-Web. Files written by a script get no MOTW, so the
# SmartScreen wall never appears; the Unblock-File below makes that
# explicit. Nothing is unverified, though - see below.
#
# Verification, in order of preference:
#   1. the release's SHA256SUMS file (published by `make publish`),
#   2. GitHub's per-asset `digest` field from the release API (HTTPS) -
#      how releases that predate SHA256SUMS still verify.
#
# Downloads, in order of size:
#   1. the standalone kvmshare-install asset (a few MB), when the
#      release carries it (v0.8.8+),
#   2. otherwise the full platform archive; the installer is extracted
#      and run with --local (works on every release).

[CmdletBinding()]
param(
    # Pin a specific release ("v0.8.7"). Default: the latest published.
    [string]$Tag,
    # Remaining arguments pass through to kvmshare-install.
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$InstallerArgs
)

$ErrorActionPreference = 'Stop'
$Repo = if ($env:KVMSHARE_UPSTREAM) { $env:KVMSHARE_UPSTREAM } else { 'YELrhilassi/kvmshare' }
$Api = "https://api.github.com/repos/$Repo/releases"
$Headers = @{ 'User-Agent' = 'kvmshare-bootstrap' }

function Say($msg) { Write-Host "kvmshare-install: $msg" }

if ($env:PROCESSOR_ARCHITECTURE -notmatch 'AMD64') {
    throw "unsupported architecture '$($env:PROCESSOR_ARCHITECTURE)' (only x64 Windows builds are published)"
}
$plat = 'windows_amd64'

# --- release metadata -------------------------------------------------------
if ($Tag) {
    Say "resolving release $Tag..."
    $rel = Invoke-RestMethod -Uri "$Api/tags/$Tag" -Headers $Headers
} else {
    Say 'resolving the latest release...'
    $rel = Invoke-RestMethod -Uri "$Api/latest" -Headers $Headers
}
$Tag = $rel.tag_name
Say "installing $Tag"

# sha256 of an asset: from SHA256SUMS (preferred) or the API digest field.
$Digests = @{}
foreach ($a in $rel.assets) {
    if ($a.digest -match '^sha256:([0-9a-f]{64})$') { $Digests[$a.name] = $Matches[1] }
}

function Get-ExpectedHash([string]$assetName) {
    # Prefer the published SHA256SUMS; fall back to the API digest field.
    try {
        $sums = Invoke-WebRequest -Uri "https://github.com/$Repo/releases/download/$Tag/SHA256SUMS" `
            -Headers $Headers -UseBasicParsing -ErrorAction Stop
        foreach ($line in $sums.Content -split "`n") {
            if ($line -match '^([0-9a-f]{64})\s+\*?(.+)$' -and $Matches[2].Trim() -eq $assetName) {
                return $Matches[1].ToLowerInvariant()
            }
        }
    } catch { }
    return $Digests[$assetName]
}

function Test-FileHash([string]$path, [string]$assetName) {
    $want = Get-ExpectedHash $assetName
    if (-not $want) { throw "no trusted digest found for $assetName (no SHA256SUMS, no API digest) - refusing to run" }
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $stream = [System.IO.File]::OpenRead($path)
        try {
            $got = ([System.BitConverter]::ToString($sha.ComputeHash($stream)) -replace '-', '').ToLowerInvariant()
        } finally { $stream.Dispose() }
    } finally { $sha.Dispose() }
    if ($got -ne $want) {
        throw "checksum mismatch for ${assetName}:`n  expected: $want`n  got:      $got`nThe release may be corrupted, or this download was tampered with."
    }
    Say "checksum ok ($assetName)"
}

$dl = "https://github.com/$Repo/releases/download/$Tag"
$installerAsset = "kvmshare-install_${Tag}_${plat}.exe"
$archiveAsset = "kvmshare_${Tag}_${plat}.zip"

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("kvmshare-install-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    $assetName = $null
    $installer = Join-Path $tmp 'kvmshare-install.exe'

    # --- path 1: standalone installer (v0.8.8+) ----------------------------
    $standalone = $rel.assets | Where-Object { $_.name -eq $installerAsset }
    if ($standalone) {
        Say "downloading $installerAsset..."
        Invoke-WebRequest -Uri "$dl/$installerAsset" -OutFile $installer -Headers $Headers -UseBasicParsing
        $assetName = $installerAsset
    } else {
        # --- path 2: full archive (any release, e.g. v0.8.7) ---------------
        Say "this release has no standalone installer asset - using the full archive"
        $zipPath = Join-Path $tmp 'release.zip'
        Say "downloading $archiveAsset..."
        Invoke-WebRequest -Uri "$dl/$archiveAsset" -OutFile $zipPath -Headers $Headers -UseBasicParsing
        Test-FileHash $zipPath $archiveAsset
        Expand-Archive -Path $zipPath -DestinationPath $tmp -Force
        $found = Get-ChildItem -Path $tmp -Recurse -Filter 'kvmshare-install.exe' | Select-Object -First 1
        if (-not $found) { throw 'archive has no kvmshare-install.exe inside - unexpected layout' }
        Copy-Item $found.FullName $installer
    }

    # --- verify + strip Mark-of-the-Web ------------------------------------
    Test-FileHash $installer $assetName
    # Staged under a script-created temp dir there is normally no MOTW;
    # Unblock-File makes that guarantee explicit (and strips anything a
    # polluted Downloads folder inherited).
    Unblock-File -Path $installer -ErrorAction SilentlyContinue

    # --- hand off ------------------------------------------------------------
    Say 'handing off to the installer...'
    & $installer @InstallerArgs
    if ($LASTEXITCODE -ne 0) { throw "kvmshare-install exited with code $LASTEXITCODE" }
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
