@echo off
setlocal

rem Find a standard Rust installation even if this terminal has an old PATH.
where cargo >nul 2>&1
if errorlevel 1 if defined CARGO_HOME set "PATH=%CARGO_HOME%\bin;%PATH%"
where cargo >nul 2>&1
if errorlevel 1 set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
where cargo >nul 2>&1
if errorlevel 1 (
    echo Rust Cargo was not found. Install Rust from https://rustup.rs/
    exit /b 1
)

pushd "%~dp0"
if errorlevel 1 exit /b 1

echo Formatting source files...
cargo fmt --all
if errorlevel 1 goto failed

echo Running tests...
cargo test --all-targets --locked
if errorlevel 1 goto failed

echo Building InstaProxy in release mode...
cargo build --release --locked
if errorlevel 1 goto failed

echo.
echo Build completed successfully.
echo Binary: %CD%\target\release\instaproxy.exe
popd
exit /b 0

:failed
echo Build stopped because the previous step failed.
popd
exit /b 1
