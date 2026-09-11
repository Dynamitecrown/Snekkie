@echo off
rem ---------------------------------------------------------------------
rem Snekkie launcher for Windows.
rem First run: creates a virtual environment and installs the app.
rem Later runs: verifies imports, then launches with no console window.
rem ---------------------------------------------------------------------
setlocal
cd /d "%~dp0"

set "VENV_PY=.venv\Scripts\python.exe"
set "VENV_PYW=.venv\Scripts\pythonw.exe"
set "CHECK=%TEMP%\snekkie_startup_error.txt"

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
    echo.
    echo Installing dependencies. Roughly 150 MB, mostly PySide6.
    echo This takes a few minutes on a first run.
    echo.
    "%VENV_PY%" -m pip install --upgrade pip || goto fail
    "%VENV_PY%" -m pip install -e . || goto fail
    echo.
    echo Setup complete.
    echo.
)

rem pythonw.exe has no console, so a startup crash would close this window
rem with no explanation. Check the imports first and report properly.
"%VENV_PY%" -c "import snekkie, PySide6, paramiko, serial, pyte" 2>"%CHECK%"
if errorlevel 1 (
    echo.
    echo   Snekkie could not start. The error was:
    echo.
    type "%CHECK%"
    echo.
    echo   If that mentions a missing module, run:
    echo       .venv\Scripts\python.exe -m pip install -e .
    echo.
    pause
    exit /b 1
)

start "" "%VENV_PYW%" -m snekkie
exit /b 0

:fail
echo.
echo   Setup failed. Scroll up for the actual error message.
echo.
pause
exit /b 1
