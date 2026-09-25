@echo off
rem ---------------------------------------------------------------------
rem Builds Snekkie on Windows:
rem   target\release\snekkie.exe        the app (runs on its own, too)
rem   dist\Snekkie-Setup-<version>.exe  the installer, if NSIS is installed
rem Needs Rust (https://rustup.rs). For the installer, also NSIS 3
rem (https://nsis.sourceforge.io, or: winget install NSIS.NSIS).
rem ---------------------------------------------------------------------
setlocal
cd /d "%~dp0"

where cargo >nul 2>&1
if errorlevel 1 (
    echo.
    echo   Rust was not found. Install it from https://rustup.rs and run this again.
    echo.
    pause
    exit /b 1
)

echo Building snekkie.exe ...
cargo build --release || goto fail

for /f "usebackq delims=" %%v in (`powershell -NoProfile -Command "(cargo metadata --no-deps --format-version 1 | ConvertFrom-Json).packages[0].version"`) do set "VERSION=%%v"

set "MAKENSIS="
where makensis >nul 2>&1 && set "MAKENSIS=makensis"
if not defined MAKENSIS if exist "%ProgramFiles(x86)%\NSIS\makensis.exe" set "MAKENSIS=%ProgramFiles(x86)%\NSIS\makensis.exe"
if not defined MAKENSIS if exist "%ProgramFiles%\NSIS\makensis.exe" set "MAKENSIS=%ProgramFiles%\NSIS\makensis.exe"
if not defined MAKENSIS (
    echo.
    echo Built target\release\snekkie.exe.
    echo NSIS was not found, so no installer was made. Install NSIS 3 to build one.
    echo.
    pause
    exit /b 0
)

if not exist dist mkdir dist
echo Building the installer for version %VERSION% ...
"%MAKENSIS%" /V2 /DVERSION=%VERSION% /DEXE=..\target\release\snekkie.exe installer\snekkie.nsi || goto fail

echo.
echo Done:
echo   target\release\snekkie.exe
echo   dist\Snekkie-Setup-%VERSION%.exe
echo.
pause
exit /b 0

:fail
echo.
echo   Build failed. Scroll up for the actual error message.
echo.
pause
exit /b 1
