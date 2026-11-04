@rem Runs fake-dsa.ps1 in place of dsa.exe; see that script.
@pwsh -NoProfile -ExecutionPolicy Bypass -File "%~dp0fake-dsa.ps1" %*
@exit /b %ERRORLEVEL%
