@echo off
setlocal
set "ROOT=%~dp0"
set "EXE=%ROOT%target\diag\victory.exe"
if not exist "%EXE%" set "EXE=%ROOT%target\release\victory.exe"
if not exist "%EXE%" if exist "%ROOT%victory.exe" set "EXE=%ROOT%victory.exe"

if not exist "%EXE%" (
    echo [VIctory_XCL] Building victory.exe for the first time ^(profile: diag^)...
    pushd "%ROOT%"
    cargo build --locked --profile diag -p victory --features diagnostics
    if errorlevel 1 (
        popd
        echo.
        echo [VIctory_XCL] Build failed. Ensure Rust ^(rustup^) and MSVC Build Tools are installed.
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
