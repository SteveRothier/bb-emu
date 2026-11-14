$ErrorActionPreference = "Stop"
$root = Resolve-Path (Join-Path $PSScriptRoot "..")
$html = Join-Path $root "scripts\icon-render.html"
$out = Join-Path $root "app-icon-source.png"

$chrome = @(
  "$env:ProgramFiles\Google\Chrome\Application\chrome.exe",
  "${env:ProgramFiles(x86)}\Google\Chrome\Application\chrome.exe",
  "$env:LocalAppData\Google\Chrome\Application\chrome.exe",
  "$env:ProgramFiles\Microsoft\Edge\Application\msedge.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1

if (-not $chrome) { throw "Chrome/Edge introuvable pour generer l'icone" }

if (Test-Path $out) { Remove-Item $out -Force }

$uri = "file:///" + ($html -replace "\\", "/")
& $chrome --headless=new --disable-gpu --virtual-time-budget=10000 --window-size=1024,1024 --screenshot="$out" $uri

$deadline = (Get-Date).AddSeconds(15)
while (-not (Test-Path $out)) {
  if ((Get-Date) -gt $deadline) { throw "Echec screenshot icone" }
  Start-Sleep -Milliseconds 200
}
Start-Sleep -Milliseconds 300
Write-Host "Wrote $out ($((Get-Item $out).Length) bytes)"
