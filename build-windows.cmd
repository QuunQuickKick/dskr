@echo off
setlocal
set "SCRIPT=%~dp0scripts\build-windows.ps1"
if not exist "%SCRIPT%" (
  echo ERROR: scripts\build-windows.ps1 was not found.
  pause
  exit /b 1
)
powershell -NoProfile -ExecutionPolicy Bypass -File "%SCRIPT%" %*
set "CODE=%ERRORLEVEL%"
if "%~1"=="" pause
exit /b %CODE%
