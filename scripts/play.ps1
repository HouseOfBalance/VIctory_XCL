# Starts the title from a game folder or executable path (saved automatically after the first run).
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
$settings = @{
    VICTORY_TEXTURE_NO_WAIT = '1'; VICTORY_RELAY = '2'; VICTORY_WIDE_BACK = '1'; VICTORY_CARD_REHASH_RUNS = '1'
    VICTORY_JOIN_COPIES = '1'; VICTORY_INDIRECT_WINDOW = '1'; VICTORY_RELAY_COPY = '1'; VICTORY_CARD_STEPS = '2'
    VICTORY_INDIRECT_BATCH = '2'; VICTORY_MAP_THREADS = '1'; VICTORY_TITLE_PRIORITIES = '0'; VICTORY_CARD_AWAKE = '1'
    VICTORY_SHOW_AFTER = '1'; VICTORY_AMBIENT = '0.8'; VICTORY_D3D12_DRED = 'light'
}
if ($Safe) {
    $settings.VICTORY_THIN = '0'; $settings.VICTORY_UNDER_WIDE = '0'; $settings.VICTORY_LAND_ECHO = '0'
    $settings.VICTORY_JOIN_BARRIERS = '0'; $settings.VICTORY_PORT_CONSTANTS = '0'; $settings.VICTORY_PORT_DESCRIPTORS = '0'
    $settings.VICTORY_PORT_CBV = '0'; $settings.VICTORY_PORT_GUARD = '0'; $settings.VICTORY_PORT_JOURNAL = '0'
} else {
    $settings.VICTORY_THIN = '1'; $settings.VICTORY_JOIN_PARTS = '8'; $settings.VICTORY_UPLOAD_AHEAD = '1'
}
if ($RayTracing) { $settings.VICTORY_BVH_BUILDER = '1' }
foreach ($k in $More.Keys) { $settings[$k] = $More[$k] }

$victory = Join-Path $PSScriptRoot "..\target\$Build\victory.exe"
if (-not (Test-Path -LiteralPath $victory)) { Write-Host "No $victory. Run Launch-VIctory.cmd or build it first (README.md)."; exit 1 }

$homeDir = if ($env:VICTORY_HOME) { $env:VICTORY_HOME } else { Join-Path $env:LOCALAPPDATA 'victory' }
$savedFile = Join-Path $homeDir 'game-path.txt'
if (-not $Exe -and -not $Pick -and (Test-Path -LiteralPath $savedFile)) {
    $candidate = (Get-Content -LiteralPath $savedFile -TotalCount 1).Trim().Trim('"').Trim("'")
    if ($candidate -and (Test-Path -LiteralPath $candidate)) { $Exe = $candidate }
}
if (-not $Exe -or $Pick) {
    Add-Type -AssemblyName System.Windows.Forms
    $form = New-Object System.Windows.Forms.Form
    $form.TopMost = $true
    $dialog = New-Object System.Windows.Forms.FolderBrowserDialog
    $dialog.Description = 'Select folder containing game .exe (Chon thu muc chua file .exe cua game)'
    $dialog.ShowNewFolderButton = $false
    if ($Exe -and (Test-Path -LiteralPath $Exe)) { $dialog.SelectedPath = $Exe }
    if ($dialog.ShowDialog($form) -eq [System.Windows.Forms.DialogResult]::OK) {
        $Exe = $dialog.SelectedPath
    }
}
if (-not $Exe -or -not (Test-Path -LiteralPath $Exe)) {
    Write-Host 'No game folder or executable selected.'
    exit 1
}
$Exe = (Resolve-Path -LiteralPath $Exe).Path
$gameDir = if (Test-Path -LiteralPath $Exe -PathType Container) { $Exe } else { Split-Path -Parent $Exe }
New-Item -ItemType Directory -Force $homeDir | Out-Null
Set-Content -LiteralPath $savedFile -Value $gameDir -Encoding UTF8

$paths = 'VICTORY_HOME', 'VICTORY_CONFIG', 'VICTORY_GAME_DIR', 'VICTORY_CACHE_DIR', 'VICTORY_DATA_DIR'
$inherited = @(Get-ChildItem Env:VICTORY_* | Where-Object { $_.Name -notin $paths })
$saved = @{}
foreach ($entry in $inherited) { $saved[$entry.Name] = $entry.Value }
$hadGameDir = [bool]$env:VICTORY_GAME_DIR
$exitCode = 0
Push-Location (Join-Path $PSScriptRoot '..')
try {
    foreach ($entry in $inherited) { Remove-Item "Env:$($entry.Name)" }
    if (-not $hadGameDir) { Set-Item Env:VICTORY_GAME_DIR $gameDir }
    foreach ($k in $settings.Keys) { Set-Item "Env:$k" $settings[$k] }
    $discarded = $inherited | Where-Object {
        $_.Name -notin $More.Keys -and -not $_.Value.Equals([Environment]::GetEnvironmentVariable($_.Name))
    } | Sort-Object Name
    if ($discarded) {
        Write-Host "Discarded inherited values for $($discarded.Name -join ', '). Use -More to pass them explicitly."
    }
    & $victory run $Exe --execute
    $exitCode = $LASTEXITCODE
} finally {
    Pop-Location
    foreach ($k in $settings.Keys) { Remove-Item "Env:$k" -ErrorAction SilentlyContinue }
    if (-not $hadGameDir) { Remove-Item Env:VICTORY_GAME_DIR -ErrorAction SilentlyContinue }
    foreach ($k in $saved.Keys) { Set-Item "Env:$k" $saved[$k] }
}
exit $exitCode
