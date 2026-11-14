# BB EMU

Launcher Tauri pour Blitz Brigade + stub UDP local (`bb-mp-stub`).

## Dev

```bash
npm install
npm run tauri:dev
```

Prérequis : Node.js, Rust, WebView2.

## Build / release

1. Monte la `version` dans `src-tauri/tauri.conf.json` (+ `package.json` / `Cargo.toml`)
2. `build.bat` (signe avec `src-tauri/keys/bb-emu.key`)
3. Crée une GitHub Release `vX.Y.Z`
4. Uploade : setup NSIS/MSI, fichiers `.sig`, et `latest.json`

Endpoint updater :

`https://github.com/SteveRothier/bb-emu/releases/latest/download/latest.json`
