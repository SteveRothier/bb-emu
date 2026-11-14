# Nettoie anciennes installs "BB EMU", raccourcis en double et cache d'icones.
$ErrorActionPreference = "Continue"
Write-Host "[BB-EMU] Nettoyage..."

# Stoppe l'app
Get-Process -Name "bb-emu" -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 400

# Ancienne install (productName avec espace)
$oldDir = Join-Path $env:LOCALAPPDATA "BB EMU"
$newDir = Join-Path $env:LOCALAPPDATA "BB-EMU"

if (Test-Path (Join-Path $oldDir "uninstall.exe")) {
  Write-Host "Desinstallation ancienne install: $oldDir"
  Start-Process (Join-Path $oldDir "uninstall.exe") -ArgumentList "/S" -Wait -ErrorAction SilentlyContinue
}
if (Test-Path $oldDir) {
  Remove-Item $oldDir -Recurse -Force -ErrorAction SilentlyContinue
  Write-Host "Supprime: $oldDir"
}

# Raccourcis qui pointent encore vers l'ancienne install
$shortcutPaths = @(
  (Join-Path $env:USERPROFILE "Desktop\BB EMU.lnk"),
  (Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\BB EMU.lnk")
)
$shell = New-Object -ComObject WScript.Shell
$newExe = Join-Path $newDir "bb-emu.exe"
foreach ($p in $shortcutPaths) {
  if (-not (Test-Path $p)) { continue }
  try {
    $lnk = $shell.CreateShortcut($p)
    if ($lnk.TargetPath -like "*\BB EMU\*") {
      if (Test-Path $newExe) {
        $lnk.TargetPath = $newExe
        $lnk.WorkingDirectory = $newDir
        $lnk.IconLocation = "$newExe,0"
        $lnk.Save()
        Write-Host "Raccourci retarget: $p"
      } else {
        Remove-Item $p -Force -ErrorAction SilentlyContinue
        Write-Host "Raccourci supprime: $p"
      }
    }
  } catch {
    Write-Host "Ignore raccourci: $p"
  }
}

# Dupliques BB-EMU / BB EMU sur le bureau : garde BB-EMU.lnk
$deskOld = Join-Path $env:USERPROFILE "Desktop\BB EMU.lnk"
$deskNew = Join-Path $env:USERPROFILE "Desktop\BB-EMU.lnk"
if ((Test-Path $deskOld) -and (Test-Path $deskNew)) {
  Remove-Item $deskOld -Force -ErrorAction SilentlyContinue
  Write-Host "Supprime doublon bureau: BB EMU.lnk"
}

# Refresh leger des icones
try { & ie4uinit.exe -show | Out-Null } catch {}
Write-Host "OK. Relance BB-EMU depuis le raccourci BB-EMU."
