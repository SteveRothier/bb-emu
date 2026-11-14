@echo off
cd /d "%~dp0"
echo BB EMU — demarrage (tauri dev)
echo Premier lancement: compilation Rust longue.
echo.
call npm run tauri dev
pause
