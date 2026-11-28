# Starts the title through victory.exe's built-in 4K 120 FPS auto-tuning launcher.
#   scripts\play.ps1                           # uses saved game folder, or opens a Windows folder picker
#   scripts\play.ps1 -Exe D:\Games\YourDump    # points to the folder containing the .exe (or the .exe itself)
#   scripts\play.ps1 -Pick                     # opens the folder picker to choose another game folder
#   scripts\play.ps1 -Safe                     # conservative mode without newer port/thin paths
#   scripts\play.ps1 -RayTracing               # turns the title's ray-traced indirect light on
#   scripts\play.ps1 -More @{VICTORY_PANEL='1'}
param(
    [Parameter(Position = 0)][string]$Exe = '',
    [switch]$Pick,
    [switch]$Safe,
    [switch]$RayTracing,
    [ValidateSet('diag', 'release')][string]$Build = 'diag',
    [hashtable]$More = @{}
)
$victory = Join-Path $PSScriptRoot "..\target\$Build\victory.exe"
if (-not (Test-Path -LiteralPath $victory) -and $Build -eq 'diag') {
    & (Join-Path $PSScriptRoot 'get-victory-exe.ps1')
}
if (-not (Test-Path -LiteralPath $victory)) {
    Write-Host "No $victory. Run Launch-VIctory.cmd or build it first (README.md)."
    exit 1
}

$cliArgs = @('play')
if ($Exe) { $cliArgs += $Exe }
if ($Pick) { $cliArgs += '--pick' }
if ($Safe) { $cliArgs += '--safe' }
if ($RayTracing) { $cliArgs += '--ray-tracing' }
foreach ($k in ($More.Keys | Sort-Object)) {
    $cliArgs += '--set'
    $cliArgs += "$k=$($More[$k])"
}

Push-Location (Join-Path $PSScriptRoot '..')
try {
    & $victory @cliArgs
    exit $LASTEXITCODE
} finally {
    Pop-Location
}
