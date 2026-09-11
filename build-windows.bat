@echo off
rem ---------------------------------------------------------------------
rem Builds dist\snekkie.exe: a standalone executable that needs no Python
rem install to run. Re-run this after pulling changes to refresh the exe.
rem ---------------------------------------------------------------------
setlocal
cd /d "%~dp0"

set "VENV_PY=.venv\Scripts\python.exe"

where python >nul 2>&1
if errorlevel 1 (
    echo.
    echo   Python was not found on your PATH.
    echo   Install Python 3.10 or newer from https://www.python.org/downloads/
    echo   and tick "Add python.exe to PATH" on the first installer screen.
    echo.
    pause
    exit /b 1
)

if not exist "%VENV_PY%" (
    echo Creating virtual environment in .venv ...
    python -m venv .venv || goto fail
)

echo Installing build dependencies ...
"%VENV_PY%" -m pip install --upgrade pip || goto fail
"%VENV_PY%" -m pip install -e ".[build]" || goto fail

echo.
echo Building dist\snekkie.exe ...
echo.
"%VENV_PY%" -m PyInstaller --noconsole --onefile --name snekkie launcher.py || goto fail

echo.
echo Done. dist\snekkie.exe is ready to run or pin to your taskbar.
echo Note: one-file builds are a common antivirus false positive. If
echo Defender quarantines it, edit this script to drop --onefile and
echo ship the dist\snekkie\ folder instead.
echo.
pause
exit /b 0

:fail
echo.
echo   Build failed. Scroll up for the actual error message.
echo.
pause
exit /b 1
