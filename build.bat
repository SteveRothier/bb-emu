@echo off
setlocal
cd /d "%~dp0"

set "TAURI_SIGNING_PRIVATE_KEY=%~dp0src-tauri\keys\bb-emu.key"
set "TAURI_SIGNING_PRIVATE_KEY_PASSWORD="

if not exist "%TAURI_SIGNING_PRIVATE_KEY%" (
  echo Cle manquante: %TAURI_SIGNING_PRIVATE_KEY%
  echo Genere-la avec: npx tauri signer generate -w src-tauri/keys/bb-emu.key --ci
  exit /b 1
)

echo [BB EMU] Build installateur + artefacts updater...
call npm run tauri:build
if errorlevel 1 (
  echo Echec du build.
  exit /b 1
)

echo.
echo Sortie installateurs:
dir /b "src-tauri\target\release\bundle\nsis\*.exe" 2>nul
dir /b "src-tauri\target\release\bundle\msi\*.msi" 2>nul
echo.
node scripts\make-latest-json.mjs
echo.
echo 1^) Uploade le setup + .sig + latest.json sur ta release
echo 2^) Mets l'URL latest.json dans tauri.conf.json -^> plugins.updater.endpoints
endlocal
