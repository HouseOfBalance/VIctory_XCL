# Downloads the latest pre-built victory.exe from GitHub Releases and verifies its SHA-256 hash
# so users do not need Rust or Visual Studio Build Tools installed locally.
param(
    [string]$Repo = 'HouseOfBalance/VIctory_XCL',
    [string]$Tag = 'latest',
    [string]$To = ''
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$dir = if ($To) { $To } else { Join-Path (Resolve-Path (Join-Path $PSScriptRoot '..')).Path 'target\diag' }
$exe = Join-Path $dir 'victory.exe'
$shaFile = Join-Path $dir 'victory.exe.sha256'
$base = "https://github.com/$Repo/releases/download/$Tag"

function Get-Download([string]$Url, [string]$Dest) {
    New-Item -ItemType Directory -Force (Split-Path $Dest) | Out-Null
    $part = "$Dest.part"
    $done = $false
    if (Get-Command curl.exe -ErrorAction SilentlyContinue) {
        & curl.exe --ssl-no-revoke --location --fail --silent --show-error --progress-bar --output $part $Url
        if ($LASTEXITCODE -eq 0 -and (Test-Path -LiteralPath $part)) { $done = $true }
        else { Remove-Item -LiteralPath $part -Force -ErrorAction SilentlyContinue }
    }
    if (-not $done) {
        Invoke-WebRequest -Uri $Url -OutFile $part -UseBasicParsing -ErrorAction Stop
    }
    Move-Item -LiteralPath $part -Destination $Dest -Force
}

Write-Host "[VIctory_XCL] Downloading pre-built victory.exe from github.com/$Repo/releases/tag/$Tag ..."
Get-Download "$base/victory.exe.sha256" $shaFile
$expected = ((Get-Content -LiteralPath $shaFile -TotalCount 1).Trim() -split '\s+')[0].ToUpperInvariant()
if ($expected.Length -ne 64) {
    throw "invalid SHA-256 file downloaded: $expected"
}

if ((Test-Path -LiteralPath $exe) -and (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash -eq $expected) {
    Write-Host "[VIctory_XCL] victory.exe is already up to date ($expected)."
    exit 0
}

$tmpExe = "$exe.download"
try {
    Get-Download "$base/victory.exe" $tmpExe
    $actual = (Get-FileHash -LiteralPath $tmpExe -Algorithm SHA256).Hash
    if ($actual -ne $expected) {
        throw "SHA-256 mismatch for victory.exe (expected $expected, got $actual)"
    }
    Move-Item -LiteralPath $tmpExe -Destination $exe -Force
} finally {
    Remove-Item -LiteralPath $tmpExe -Force -ErrorAction SilentlyContinue
}

$mb = (Get-Item -LiteralPath $exe).Length / 1MB
Write-Host ('[VIctory_XCL] Ready: {0} ({1:N1} MB, SHA-256 verified)' -f $exe, $mb)
