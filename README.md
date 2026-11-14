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
2. `build.bat` — build, signe, génère `latest.json` et publie la GitHub Release automatiquement (`gh` requis)

Endpoint updater :

`https://github.com/SteveRothier/bb-emu/releases/latest/download/latest.json`

## Nettoyage

`cleanup.bat` — désinstalle l’ancienne install `BB EMU`, corrige les raccourcis en double, refresh des icônes.

## Logs

Écrits dans le dossier logs Tauri de l’app (`bb-emu.log`) : démarrage stub, lobby, détection jeu, erreurs.
