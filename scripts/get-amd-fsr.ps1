# Fetches AMD FidelityFX SDK files for FSR 3 Frame Generation (about 30 MB) from AMD's official GitHub
# repository (GPUOpen-LibrariesAndSDKs/FidelityFX-SDK) into %LOCALAPPDATA%\victory\amd-fidelityfx (via
# `victory get-fsr` or `victory play`) or amd-sdk\FidelityFX\bin locally (git-ignored).
# Each file is verified by SHA-256 and checked for AMD's Authenticode signature at runtime.
param(
    [string]$Tag = 'v2.4.0',
    [string]$To = ''
)
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
$bin = if ($To) { $To } else { Join-Path (Resolve-Path (Join-Path $PSScriptRoot '..')).Path 'amd-sdk\FidelityFX\bin' }
$base = "https://raw.githubusercontent.com/GPUOpen-LibrariesAndSDKs/FidelityFX-SDK/$Tag/Kits/FidelityFX"
$files = [ordered]@{
    'amd_fidelityfx_loader_dx12.dll'          = 'E2191D3C0828BE45EB8AFFD4E3EC0378F2293ADC345B772A7170EDCF3B021ECB'
    'amd_fidelityfx_framegeneration_dx12.dll' = 'E6843252F58B0341A0D80F6A20BC0D840643EE95BC48C22CAC08745344577BB6'
}

function Get-File([string]$Url, [string]$To) {
    New-Item -ItemType Directory -Force (Split-Path $To) | Out-Null
    $part = "$To.part"
    $done = $false
    if (Get-Command curl.exe -ErrorAction SilentlyContinue) {
        & curl.exe --ssl-no-revoke --location --fail --silent --show-error --progress-bar --output $part $Url
        if ($LASTEXITCODE -eq 0 -and (Test-Path -LiteralPath $part)) { $done = $true }
        else { Remove-Item -LiteralPath $part -Force -ErrorAction SilentlyContinue }
    }
    if (-not $done) {
        Invoke-WebRequest -Uri $Url -OutFile $part -UseBasicParsing -ErrorAction Stop
    }
    $part
}

Write-Host "AMD FSR 3 Frame Generation (FidelityFX SDK $Tag), from github.com/GPUOpen-LibrariesAndSDKs/FidelityFX-SDK (about 30 MB):"
New-Item -ItemType Directory -Force $bin | Out-Null
foreach ($name in $files.Keys) {
    $to = Join-Path $bin $name
    if ((Test-Path -LiteralPath $to) -and (Get-FileHash -LiteralPath $to -Algorithm SHA256).Hash -eq $files[$name]) {
        Write-Host "  $name already there"
        continue
    }
    $part = Get-File "$base/signedbin/$name" $to
    $hash = (Get-FileHash -LiteralPath $part -Algorithm SHA256).Hash
    if ($hash -ne $files[$name]) {
        Remove-Item -LiteralPath $part -Force
        throw "$name SHA-256 ($hash) does not match expected $($files[$name]); not kept"
    }
    Move-Item -LiteralPath $part -Destination $to -Force
    $len = (Get-Item -LiteralPath $to).Length
    Write-Host ("  {0}  {1}" -f $name, $(if ($len -ge 1MB) { '{0:N1} MB' -f ($len / 1MB) } else { '{0:N0} KB' -f ($len / 1KB) }))
}
try {
    $lic = Get-File "$base/docs/license.md" (Join-Path $bin 'amd-fidelityfx-license.md')
    Move-Item -LiteralPath $lic -Destination (Join-Path $bin 'amd-fidelityfx-license.md') -Force
    Write-Host "  licence: $(Join-Path $bin 'amd-fidelityfx-license.md')"
} catch { Write-Host "  (AMD's licence text could not be fetched: $($_.Exception.Message))" }
Write-Host 'Done. They are used from the next start.'
