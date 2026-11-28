# Fetches NVIDIA Streamline SDK files for DLSS Super Resolution and DLSS Frame Generation (about 70 MB)
# directly from NVIDIA's official GitHub release into %LOCALAPPDATA%\victory\nvidia-sdk (when invoked via
# `victory get-dlss` or `victory play`) or nvidia-sdk\Streamline\bin\x64 locally (git-ignored).
# Every DLL loaded at runtime is verified against NVIDIA's Authenticode signature before loading.
param(
    [string]$StreamlineTag = 'v2.14.16',
    [string]$To = ''
)
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
$sdk = if ($To) { $To } else { Join-Path (Resolve-Path (Join-Path $PSScriptRoot '..')).Path 'nvidia-sdk' }
$sl = Join-Path $sdk 'Streamline'
$bin = Join-Path $sl 'bin\x64'
$dlls = 'sl.interposer.dll', 'sl.common.dll', 'sl.dlss.dll', 'sl.dlss_g.dll', 'sl.reflex.dll', 'sl.pcl.dll', 'nvngx_dlss.dll', 'nvngx_dlssg.dll'

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
    Move-Item -LiteralPath $part -Destination $To -Force
}

function Get-Range([string]$Url, [long]$First, [long]$Last) {
    $q = [Net.HttpWebRequest]::Create($Url)
    $q.UserAgent = 'victory-get-nvidia-sdks'
    $q.AddRange($First, $Last)
    $r = $q.GetResponse()
    try {
        if ([int]$r.StatusCode -ne 206) { throw 'the server does not support HTTP range requests' }
        $m = New-Object IO.MemoryStream
        $r.GetResponseStream().CopyTo($m)
        , $m.ToArray()
    } finally { $r.Close() }
}

function Get-FromZip([string]$Url, [string]$Want, [string]$Into) {
    $h = [Net.HttpWebRequest]::Create($Url); $h.Method = 'HEAD'; $h.UserAgent = 'victory-get-nvidia-sdks'
    $r = $h.GetResponse(); $real = $r.ResponseUri.AbsoluteUri; $length = $r.ContentLength; $r.Close()
    $tailLen = [Math]::Min($length, 2MB)
    $tail = Get-Range $real ($length - $tailLen) ($length - 1)
    $end = -1
    for ($i = $tail.Length - 22; $i -ge 0; $i--) {
        if ($tail[$i] -eq 0x50 -and $tail[$i + 1] -eq 0x4b -and $tail[$i + 2] -eq 5 -and $tail[$i + 3] -eq 6) { $end = $i; break }
    }
    if ($end -lt 0) { throw 'no zip directory found' }
    $dirSize = [BitConverter]::ToUInt32($tail, $end + 12)
    $dirAt = [BitConverter]::ToUInt32($tail, $end + 16)
    if ($dirAt -eq [uint32]::MaxValue) { throw 'a zip64 archive' }
    $dir = if ($dirAt -ge $length - $tailLen) {
        $from = [int]($dirAt - ($length - $tailLen)); , $tail[$from..($from + $dirSize - 1)]
    } else { Get-Range $real $dirAt ($dirAt + $dirSize - 1) }
    $got = 0
    $at = 0
    while ($at + 46 -le $dir.Length -and [BitConverter]::ToUInt32($dir, $at) -eq 0x02014b50) {
        $method = [BitConverter]::ToUInt16($dir, $at + 10)
        $packed = [BitConverter]::ToUInt32($dir, $at + 20)
        $size = [BitConverter]::ToUInt32($dir, $at + 24)
        $nameLen = [BitConverter]::ToUInt16($dir, $at + 28)
        $extraLen = [BitConverter]::ToUInt16($dir, $at + 30)
        $noteLen = [BitConverter]::ToUInt16($dir, $at + 32)
        $headerAt = [BitConverter]::ToUInt32($dir, $at + 42)
        $name = [Text.Encoding]::UTF8.GetString($dir, $at + 46, $nameLen)
        $at += 46 + $nameLen + $extraLen + $noteLen
        if ($name -notmatch $Want) { continue }
        $piece = Get-Range $real $headerAt ($headerAt + 30 + $nameLen + 2048 + $packed - 1)
        if ([BitConverter]::ToUInt32($piece, 0) -ne 0x04034b50) { throw "no file header for $name" }
        $dataAt = 30 + [BitConverter]::ToUInt16($piece, 26) + [BitConverter]::ToUInt16($piece, 28)
        if ($dataAt + $packed -gt $piece.Length) { throw "the header of $name is longer than expected" }
        $out = [IO.MemoryStream]::new()
        if ($method -eq 0) { $out.Write($piece, $dataAt, [int]$packed) }
        elseif ($method -eq 8) {
            $in = [IO.MemoryStream]::new($piece, $dataAt, [int]$packed)
            $z = [IO.Compression.DeflateStream]::new($in, [IO.Compression.CompressionMode]::Decompress)
            $z.CopyTo($out); $z.Close()
        } else { throw "$name uses unsupported compression method ($method)" }
        if ($out.Length -ne $size) { throw "$name extracted $($out.Length) bytes, expected $size" }
        $rel = ($name -replace '^.*?(?=(bin/x64/|license\.txt$))', '') -replace '/', '\'
        $dest = Join-Path $Into $rel
        New-Item -ItemType Directory -Force (Split-Path $dest) | Out-Null
        [IO.File]::WriteAllBytes($dest, $out.ToArray())
        Write-Host ("  {0}  {1:N1} MB" -f $rel, ($size / 1MB))
        $got++
    }
    $got
}

Write-Host "NVIDIA Streamline $StreamlineTag with DLSS and Frame Generation, from github.com/NVIDIA-RTX/Streamline (about 70 MB):"
$missing = @($dlls | Where-Object { -not (Test-Path -LiteralPath (Join-Path $bin $_)) })
if (-not $missing) {
    Write-Host "  all there already: $bin"
} else {
    $url = "https://github.com/NVIDIA-RTX/Streamline/releases/download/$StreamlineTag/streamline-sdk-$StreamlineTag.zip"
    $names = ($dlls | ForEach-Object { [regex]::Escape($_) }) -join '|'
    $want = "(^|/)bin/x64/($names|[^/]*license[^/]*\.txt)$|^([^/]+/)?license\.txt$"
    try { [void](Get-FromZip $url $want $sl) }
    catch { Write-Host "  Range extraction did not succeed ($($_.Exception.Message))." }
    if (@($dlls | Where-Object { -not (Test-Path -LiteralPath (Join-Path $bin $_)) })) {
        Write-Host '  Downloading full archive instead (263 MB) ...'
        $zip = Join-Path $sdk "streamline-sdk-$StreamlineTag.zip"
        Get-File $url $zip
        $into = Join-Path $sdk 'streamline-unpacked'
        Expand-Archive -LiteralPath $zip -DestinationPath $into -Force -ErrorAction Stop
        $found = Get-ChildItem -LiteralPath $into -Recurse -Filter 'sl.interposer.dll' |
            Where-Object { $_.FullName -match '\\bin\\x64\\sl\.interposer\.dll$' -and $_.FullName -notmatch '\\development\\' } |
            Select-Object -First 1
        if (-not $found) { throw 'The Streamline archive has no bin\x64\sl.interposer.dll.' }
        $top = Split-Path (Split-Path (Split-Path $found.FullName))
        New-Item -ItemType Directory -Force $bin | Out-Null
        Get-ChildItem -LiteralPath (Join-Path $top 'bin\x64') -File |
            Where-Object { $dlls -contains $_.Name -or $_.Name -match 'license.*\.txt$' } |
            Copy-Item -Destination $bin -Force
        Copy-Item -LiteralPath (Join-Path $top 'license.txt') -Destination $sl -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $into -Recurse -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $zip -Force
    }
}
$still = @($dlls | Where-Object { -not (Test-Path -LiteralPath (Join-Path $bin $_)) })
if ($still) { throw "Missing Streamline files: $($still -join ', ')" }
Write-Host "  licences: $(Join-Path $sl 'license.txt') and *.license.txt in $bin"
Write-Host 'Done. They are used from the next start.'
