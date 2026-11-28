@echo off
setlocal
set "ROOT=%~dp0"
if exist "%USERPROFILE%\.cargo\bin\cargo.exe" set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

set "EXE=%ROOT%target\diag\victory.exe"
if not exist "%EXE%" if exist "%ROOT%target\release\victory.exe" set "EXE=%ROOT%target\release\victory.exe"
if not exist "%EXE%" if exist "%ROOT%victory.exe" set "EXE=%ROOT%victory.exe"

if not exist "%EXE%" (
    echo [VIctory_XCL] Pre-built victory.exe not found locally. Downloading latest release from GitHub...
    powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%ROOT%scripts\get-victory-exe.ps1"
    if exist "%ROOT%target\diag\victory.exe" set "EXE=%ROOT%target\diag\victory.exe"
)

if not exist "%EXE%" (
    where cargo >nul 2>nul
    if errorlevel 1 (
        echo.
        echo [VIctory_XCL] Could not download pre-built victory.exe ^(check internet connection or GitHub Releases^)
        echo               and Rust ^(cargo^) is not installed locally.
        pause
        exit /b 1
    )
    echo [VIctory_XCL] Building victory.exe locally ^(profile: diag^)...
    pushd "%ROOT%"
    cargo build --locked --profile diag -p victory --features diagnostics
    if errorlevel 1 (
        popd
        echo.
        echo [VIctory_XCL] Local build failed.
        pause
        exit /b 1
    )
    popd
    set "EXE=%ROOT%target\diag\victory.exe"
)

pushd "%ROOT%"
"%EXE%" play %*
set "CODE=%ERRORLEVEL%"
popd
if not "%CODE%"=="0" (
    echo.
    pause
)
exit /b %CODE%
