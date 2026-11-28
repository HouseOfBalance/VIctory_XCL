# Fetches Microsoft's Direct3D 12 Agility SDK runtime (D3D12Core.dll, from Microsoft's NuGet package
# Microsoft.Direct3D.D3D12, about 22 MB) into a D3D12 folder beside victory.exe. Windows 10's built-in
# Direct3D 12 stops at shader model 6.5 while translated shaders require 6.6. Both the package and the
# extracted DLL are verified by SHA-256 and Microsoft Authenticode signature before keeping.
param(
    [string]$Version = '1.614.1',
    [string]$To = ''
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
$dir = if ($To) { $To } else { Join-Path (Resolve-Path (Join-Path $PSScriptRoot '..')).Path 'target\diag\D3D12' }
$core = '8A23D826B25B4329522FF451CB52B7F2B34D7F2913CFEB878371CE8BD765FE2D'
$package = '9880AA91602DD51DD6CF7911A2BCA7A2323513B15338573CDE014B3356EEAFF2'
$url = "https://api.nuget.org/v3-flatcontainer/microsoft.direct3d.d3d12/$Version/microsoft.direct3d.d3d12.$Version.nupkg"

$target = Join-Path $dir 'D3D12Core.dll'
if ((Test-Path -LiteralPath $target) -and (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash -eq $core) {
    Write-Host "Microsoft's Direct3D 12 runtime $Version is already in $dir"
    exit 0
}
Write-Host "Microsoft's Direct3D 12 runtime $Version (Agility SDK), from nuget.org (about 22 MB):"
New-Item -ItemType Directory -Force $dir | Out-Null
$part = Join-Path $dir "microsoft.direct3d.d3d12.$Version.nupkg.part"
try {
    if (Get-Command curl.exe -ErrorAction SilentlyContinue) {
        & curl.exe --location --fail --silent --show-error --progress-bar --output $part $url
        if ($LASTEXITCODE -ne 0) { throw "could not get $url" }
    } else {
        Invoke-WebRequest -Uri $url -OutFile $part -UseBasicParsing
    }
    $hash = (Get-FileHash -LiteralPath $part -Algorithm SHA256).Hash
    if ($hash -ne $package) { throw "the package SHA-256 ($hash) does not match expected $package; not kept" }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead($part)
    try {
        $entry = $zip.Entries | Where-Object FullName -eq 'build/native/bin/x64/D3D12Core.dll'
        if (-not $entry) { throw 'the package has no build/native/bin/x64/D3D12Core.dll' }
        $dll = "$target.part"
        [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $dll, $true)
        $lic = $zip.Entries | Where-Object FullName -eq 'LICENSE.txt'
        if ($lic) { [IO.Compression.ZipFileExtensions]::ExtractToFile($lic, (Join-Path $dir 'microsoft-d3d12-license.txt'), $true) }
    } finally { $zip.Dispose() }
    $hash = (Get-FileHash -LiteralPath $dll -Algorithm SHA256).Hash
    if ($hash -ne $core) { Remove-Item -LiteralPath $dll -Force; throw "D3D12Core.dll SHA-256 ($hash) does not match expected $core; not kept" }
    $sig = Get-AuthenticodeSignature -LiteralPath $dll
    if ($sig.Status -ne 'Valid' -or $sig.SignerCertificate.Subject -notlike 'CN=Microsoft Corporation,*') {
        Remove-Item -LiteralPath $dll -Force; throw "D3D12Core.dll is not validly signed by Microsoft ($($sig.Status)); not kept"
    }
    Move-Item -LiteralPath $dll -Destination $target -Force
} finally {
    Remove-Item -LiteralPath $part -Force -ErrorAction SilentlyContinue
}
Write-Host ('  D3D12Core.dll  {0:N1} MB, signed by Microsoft' -f ((Get-Item -LiteralPath $target).Length / 1MB))
Write-Host "  in $dir (licence: microsoft-d3d12-license.txt beside it)"
