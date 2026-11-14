# BB EMU

Launcher Windows pour **Blitz Brigade** (stub UDP local inclus).

## Installation

1. Ouvre la **[dernière release](https://github.com/SteveRothier/bb-emu/releases/latest)**
2. Télécharge **`BB-EMU_…_x64-setup.exe`**
3. Lance l’installateur (pas besoin d’admin)
4. Ouvre **BB-EMU** → Config si besoin (`BlitzBrigade.exe`) → **PLAY**

Prérequis : [Node.js](https://nodejs.org/) (stub) + WebView2 (déjà là sur Windows 10/11 récents).

Les mises à jour s’installent automatiquement au lancement.

---

## Dev

```bash
npm install
npm run tauri:dev
```

Prérequis : Node.js, Rust, WebView2.

## Build / release

1. Monte la `version` dans `src-tauri/tauri.conf.json` (+ `package.json` / `Cargo.toml`)
2. `build.bat` — build, signe, génère `latest.json` et publie la GitHub Release (`gh` requis)

Endpoint updater :  
`https://github.com/SteveRothier/bb-emu/releases/latest/download/latest.json`

## Nettoyage

`cleanup.bat` — désinstalle l’ancienne install `BB EMU`, corrige les raccourcis en double, refresh des icônes.

## Logs

Écrits dans le dossier logs Tauri de l’app (`bb-emu.log`) : démarrage stub, lobby, détection jeu, erreurs.
