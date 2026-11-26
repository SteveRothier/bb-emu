import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import {
  createElement,
  LoaderCircle,
  Minus,
  Settings,
  FolderOpen,
  X,
  CircleCheck,
  CircleAlert,
} from "lucide";
import type { IconNode } from "lucide";
import "./styles.css";

type Config = {
  gamePath: string;
  lastLobbyName: string;
  lastMapId: number;
  lastGameMode: number;
};

const DEFAULT_CONFIG: Config = {
  gamePath: "",
  lastLobbyName: "MaPartie",
  lastMapId: 0, // Cathédrale (wire)
  lastGameMode: 0, // Domination
};

/**
 * Wire 0x02 name = [gameMode, playerCount, maxPlayers, mapId] (voir levels.js).
 * Modes : Dom=0, CTF=1, DM=2, FFA=3. Cartes : Cat=0 … OVNI=5.
 */
const MAP_OPTIONS = [
  { id: 0, label: "Cathédrale" },
  { id: 1, label: "Madagascar" },
  { id: 2, label: "Ruines de Madagascar" },
  { id: 3, label: "Quais de Malte" },
  { id: 4, label: "Fort de Malte" },
  { id: 5, label: "Crash OVNI" },
];

const MODE_OPTIONS = [
  { id: 0, label: "Domination" },
  { id: 1, label: "Capture de drapeau" },
  { id: 2, label: "Match à mort" },
  { id: 3, label: "Chacun pour soi" },
];

/** Cartes autorisées par mode (ids wire). */
const MAPS_BY_MODE: Record<number, number[]> = {
  0: [0, 1, 2, 4, 5],
  1: [0, 1, 2, 3, 5],
  2: [0, 2, 3, 4, 5],
  3: [0, 2, 3, 5],
};

function resolveLevelIndex(modeId: number, mapId: number): number {
  return modeId * 6 + mapId;
}

function levelLabel(modeId: number, mapId: number): string {
  return `${labelOf(MODE_OPTIONS, modeId)} · ${labelOf(MAP_OPTIONS, mapId)}`;
}

const LOBBY_MAX_LEN = 16;

const win = getCurrentWindow();

function icon(node: IconNode, size = 16): SVGElement {
  const el = createElement(node);
  el.setAttribute("width", String(size));
  el.setAttribute("height", String(size));
  el.setAttribute("stroke-width", "2");
  return el;
}

function mountIcon(target: Element, node: IconNode, size = 16) {
  target.replaceChildren(icon(node, size));
}

function loadConfig(): Config {
  try {
    const raw = localStorage.getItem("bb-emu-config");
    if (!raw) return { ...DEFAULT_CONFIG };
    const parsed = JSON.parse(raw) as Partial<Config>;
    return {
      gamePath: String(parsed.gamePath || ""),
      lastLobbyName: String(parsed.lastLobbyName || DEFAULT_CONFIG.lastLobbyName).slice(
        0,
        LOBBY_MAX_LEN,
      ),
      lastMapId: Math.max(0, Math.min(255, Number(parsed.lastMapId) || 0)),
      lastGameMode: Math.max(0, Math.min(255, Number(parsed.lastGameMode) || 0)),
    };
  } catch {
    return { ...DEFAULT_CONFIG };
  }
}

function saveConfig(cfg: Config) {
  localStorage.setItem("bb-emu-config", JSON.stringify(cfg));
}

function isStorePath(p: string): boolean {
  const s = p.toLowerCase();
  return (
    s.includes("windowsapps") &&
    (s.includes("gameloftsa.blitzbrigade") || s.includes("infamousheroesentry.exe"))
  );
}

function shortPath(p: string): string {
  if (!p || isStorePath(p)) return "Blitz Brigade (Store)";
  const parts = p.replace(/\//g, "\\").split("\\");
  if (parts.length <= 2) return p;
  return `…\\${parts.slice(-2).join("\\")}`;
}

function labelOf(options: { id: number; label: string }[], id: number): string {
  return options.find((o) => o.id === id)?.label ?? String(id);
}

const app = document.querySelector("#app")!;

app.innerHTML = `
  <div class="shell">
    <header class="titlebar">
      <div class="titlebar-drag" data-tauri-drag-region></div>
      <div class="titlebar-controls">
        <button class="win-btn" id="win-min" type="button" aria-label="Réduire"></button>
        <button class="win-btn close" id="win-close" type="button" aria-label="Fermer"></button>
      </div>
    </header>

    <div class="stage">
      <section class="panel">
        <div class="brand-block" aria-label="BB EMU">
          <span class="brand-bb">BB</span>
          <span class="brand-emu">EMU</span>
        </div>

        <div class="dock">
          <div class="actions-row">
            <div class="actions bb-duo">
              <button class="bb-btn bb-duo-play" id="btn-launch" type="button" disabled>
                <span id="play-label">Lancer</span>
              </button>
              <button class="bb-btn bb-duo-config" id="btn-config" type="button" aria-label="Configurer">
                <span id="ico-config"></span>
              </button>
            </div>
          </div>
          <div class="dock-slot">
            <div class="boot-panel" id="boot-panel">
              <span class="boot-tag" id="boot-tag">MAJ</span>
              <div class="boot-bar" aria-hidden="true">
                <div class="boot-bar-fill" id="boot-bar-fill"></div>
              </div>
            </div>
            <div class="status" id="status-row" hidden>
              <span class="status-icon" id="status-dot"></span>
              <span id="status-text">Prêt</span>
            </div>
          </div>
        </div>
      </section>
      <div class="art-space" aria-hidden="true"></div>

      <aside class="config" id="config" aria-hidden="true">
        <div class="cfg-band cfg-band-path">
          <span class="cfg-band-label">Jeu</span>
          <button type="button" class="cfg-path-btn" id="cfg-game-btn" title="Jeu Microsoft Store">
            <span id="cfg-game-text"></span>
            <span id="ico-folder"></span>
          </button>
        </div>
        <button class="bb-btn bb-tan bb-sm cfg-store-btn" id="btn-store" type="button" hidden>
          <span>Installer le jeu (Store)</span>
        </button>

        <p class="cfg-kicker">Partie LAN</p>
        <label class="cfg-band cfg-band-name">
          <span class="cfg-name-head">
            <span class="cfg-band-label">Nom</span>
            <span class="cfg-name-count" id="cfg-name-count">0/${LOBBY_MAX_LEN}</span>
          </span>
          <input id="cfg-name" type="text" maxlength="${LOBBY_MAX_LEN}" autocomplete="off" spellcheck="false" />
        </label>
        <label class="cfg-band cfg-band-select">
          <span class="cfg-band-label">Carte</span>
          <select id="cfg-map" aria-label="Carte"></select>
        </label>
        <label class="cfg-band cfg-band-select">
          <span class="cfg-band-label">Mode</span>
          <select id="cfg-mode" aria-label="Mode"></select>
        </label>

        <div class="config-actions">
          <button class="bb-btn bb-tan bb-sm" id="btn-party-toggle" type="button">
            <span id="btn-party-toggle-label">Créer</span>
          </button>
          <button class="bb-btn bb-tan bb-sm" id="btn-close" type="button">
            <span>Fermer</span>
          </button>
        </div>
      </aside>
    </div>
    <p class="version-badge" id="app-version">v…</p>
  </div>
`;

mountIcon(document.querySelector("#win-min")!, Minus, 14);
mountIcon(document.querySelector("#win-close")!, X, 14);
mountIcon(document.querySelector("#ico-folder")!, FolderOpen, 16);
{
  const cfgIco = document.querySelector("#ico-config")!;
  mountIcon(cfgIco, Settings, 24);
  const svg = cfgIco.querySelector("svg");
  if (svg) {
    svg.setAttribute("stroke-width", "2");
    svg.setAttribute("stroke-linecap", "round");
    svg.setAttribute("stroke-linejoin", "round");
  }
}

const btnLaunch = document.querySelector<HTMLButtonElement>("#btn-launch")!;
const btnConfig = document.querySelector<HTMLButtonElement>("#btn-config")!;
const btnClose = document.querySelector<HTMLButtonElement>("#btn-close")!;
const btnPartyToggle = document.querySelector<HTMLButtonElement>("#btn-party-toggle")!;
const btnPartyToggleLabel = document.querySelector<HTMLElement>("#btn-party-toggle-label")!;
const configEl = document.querySelector<HTMLElement>("#config")!;
const cfgName = document.querySelector<HTMLInputElement>("#cfg-name")!;
const cfgNameCount = document.querySelector<HTMLElement>("#cfg-name-count")!;
const cfgMap = document.querySelector<HTMLSelectElement>("#cfg-map")!;
const cfgMode = document.querySelector<HTMLSelectElement>("#cfg-mode")!;
const cfgGameBtn = document.querySelector<HTMLButtonElement>("#cfg-game-btn")!;
const cfgGameText = document.querySelector<HTMLElement>("#cfg-game-text")!;
const btnStore = document.querySelector<HTMLButtonElement>("#btn-store")!;
const statusRow = document.querySelector<HTMLElement>("#status-row")!;
const statusDot = document.querySelector<HTMLElement>("#status-dot")!;
const statusText = document.querySelector<HTMLElement>("#status-text")!;
const bootPanel = document.querySelector<HTMLElement>("#boot-panel")!;
const bootBarFill = document.querySelector<HTMLElement>("#boot-bar-fill")!;
const appVersionEl = document.querySelector<HTMLElement>("#app-version")!;

let partyLive = false;
/** Dernière config poussée au stub (pour détecter un brouillon périmé). */
let activeParty: {
  name: string;
  mapId: number;
  gameMode: number;
  levelId: number;
} | null = null;

function lobbyKey(l: {
  name: string;
  mapId: number;
  gameMode: number;
  levelId: number;
}) {
  return `${l.name}|${l.levelId}|${l.mapId}|${l.gameMode}`;
}

async function ensurePartyWithDraft(lobby: {
  name: string;
  mapId: number;
  gameMode: number;
  levelId: number;
}) {
  const same =
    partyLive && activeParty != null && lobbyKey(activeParty) === lobbyKey(lobby);
  if (same) return;

  if (partyLive) {
    await invoke("stop_stub");
    partyLive = false;
    activeParty = null;
    syncPartyToggle();
  }

  await invoke("start_party", {
    port: 7891,
    lobbyName: lobby.name,
    levelId: lobby.levelId,
    mapId: lobby.mapId,
    maxPlayers: 8,
    gameMode: lobby.gameMode,
  });
  partyLive = true;
  activeParty = { ...lobby };
  syncPartyToggle();
  await syncLobbyInjectQuiet();
}

async function syncLobbyInjectQuiet() {
  try {
    await invoke("sync_lobby_inject");
  } catch {
    // jeu pas encore patché / pas lancé — ignore
  }
}

for (const m of MODE_OPTIONS) {
  const opt = document.createElement("option");
  opt.value = String(m.id);
  opt.textContent = m.label;
  cfgMode.append(opt);
}
refillMapOptions(0, 0);

cfgMode.addEventListener("change", () => {
  refillMapOptions(Number(cfgMode.value) || 0);
});

function syncNameCount() {
  cfgNameCount.textContent = `${cfgName.value.length}/${LOBBY_MAX_LEN}`;
}

function refillMapOptions(modeId: number, preferredMapId?: number) {
  const allowed = MAPS_BY_MODE[modeId] || MAPS_BY_MODE[0];
  const prev = preferredMapId ?? Number(cfgMap.value);
  cfgMap.replaceChildren();
  for (const id of allowed) {
    const m = MAP_OPTIONS.find((x) => x.id === id);
    if (!m) continue;
    const opt = document.createElement("option");
    opt.value = String(m.id);
    opt.textContent = m.label;
    cfgMap.append(opt);
  }
  cfgMap.value = allowed.includes(prev) ? String(prev) : String(allowed[0]);
}

function fillDraftFromConfig() {
  cfgName.value = (config.lastLobbyName || DEFAULT_CONFIG.lastLobbyName).slice(0, LOBBY_MAX_LEN);
  const modeId = MODE_OPTIONS.some((m) => m.id === config.lastGameMode)
    ? config.lastGameMode
    : 0;
  cfgMode.value = String(modeId);
  refillMapOptions(modeId, config.lastMapId ?? 0);
  syncNameCount();
}

function readLobbyDraft() {
  let name = cfgName.value.trim() || "MaPartie";
  name = [...name].slice(0, LOBBY_MAX_LEN).join("");
  const mapId = Math.max(0, Math.min(255, Number(cfgMap.value) || 0));
  const gameMode = Math.max(0, Math.min(255, Number(cfgMode.value) || 0));
  const levelId = resolveLevelIndex(gameMode, mapId);
  return { name, mapId, gameMode, levelId };
}

function syncPartyToggle() {
  btnPartyToggle.disabled = booting || busy;
  btnPartyToggleLabel.textContent = partyLive ? "Arrêter" : "Créer";
  btnPartyToggle.classList.toggle("is-stop", partyLive);
  btnPartyToggle.title = partyLive
    ? "Arrêter la partie LAN"
    : "Créer la partie LAN avec ces réglages";
  btnConfig.classList.toggle("is-party-live", partyLive);
}

async function refreshPartyStatus() {
  try {
    const status = await invoke<{ running: boolean }>("get_status");
    partyLive = !!status.running;
  } catch {
    partyLive = false;
  }
  if (!partyLive) activeParty = null;
  syncPartyToggle();
}

document.querySelector("#win-min")!.addEventListener("click", () => win.minimize());
document.querySelector("#win-close")!.addEventListener("click", () => win.close());

let config = loadConfig();
let draftPath = config.gamePath;
let busy = false;

cfgGameText.textContent = shortPath(draftPath);

void getVersion()
  .then((v) => {
    appVersionEl.textContent = `v${v}`;
  })
  .catch(() => {
    appVersionEl.textContent = "v?";
  });

let booting = true;

function hasGameInstalled() {
  return true;
}

function syncPlayBtn() {
  btnLaunch.disabled = booting || busy || !hasGameInstalled();
  btnLaunch.title = btnLaunch.disabled ? "Installe le jeu pour lancer" : "Ouvrir le jeu";
  syncPartyToggle();
}

function setBoot(pct: number) {
  bootPanel.hidden = false;
  statusRow.hidden = true;
  bootBarFill.style.width = `${Math.max(0, Math.min(100, pct))}%`;
  syncPlayBtn();
}

function finishBoot() {
  booting = false;
  bootPanel.hidden = true;
  statusRow.hidden = true;
  syncPlayBtn();
  void refreshPartyStatus();
}

function setStatus(kind: "idle" | "ok" | "err" | "busy", message: string) {
  if (booting) return;
  if (kind === "idle") {
    statusRow.hidden = true;
    busy = false;
    syncPlayBtn();
    return;
  }
  statusRow.hidden = false;
  statusDot.classList.remove("ok", "err", "busy");
  const iconMap = {
    ok: CircleCheck,
    err: CircleAlert,
    busy: LoaderCircle,
  } as const;
  statusDot.classList.add(kind);
  mountIcon(statusDot, iconMap[kind], 16);
  const svg = statusDot.querySelector("svg");
  if (svg) {
    svg.setAttribute("stroke-width", "2.25");
    svg.setAttribute("stroke-linecap", "round");
    svg.setAttribute("stroke-linejoin", "round");
  }
  statusText.textContent = message;
  busy = kind === "busy";
  syncPlayBtn();
}

function isConfigOpen() {
  return configEl.classList.contains("open");
}

function syncStoreBtn() {
  const missing = !hasGameInstalled();
  btnStore.hidden = !missing;
  syncPlayBtn();
}

function openConfig(open: boolean) {
  if (open) {
    draftPath = config.gamePath;
    cfgGameText.textContent = shortPath(draftPath);
    fillDraftFromConfig();
    syncStoreBtn();
    syncPartyToggle();
    void autoDetectGame();
  }
  configEl.classList.toggle("open", open);
  configEl.setAttribute("aria-hidden", open ? "false" : "true");
  btnConfig.setAttribute("aria-expanded", open ? "true" : "false");
}

btnConfig.setAttribute("aria-expanded", "false");
btnConfig.addEventListener("click", (e) => {
  e.stopPropagation();
  openConfig(!isConfigOpen());
});
btnClose.addEventListener("click", () => openConfig(false));

cfgName.addEventListener("input", () => {
  cfgName.value = [...cfgName.value].slice(0, LOBBY_MAX_LEN).join("");
  syncNameCount();
});

document.addEventListener("pointerdown", (e) => {
  if (!isConfigOpen()) return;
  const t = e.target as Node;
  if (configEl.contains(t) || btnConfig.contains(t)) return;
  openConfig(false);
});

cfgGameBtn.addEventListener("click", async (e) => {
  e.preventDefault();
  e.stopPropagation();
  const path = draftPath || config.gamePath;
  if (path && !isStorePath(path)) {
    try {
      await invoke("reveal_game_path", { path });
    } catch (err) {
      flashStatus("err", shortErr(err instanceof Error ? err.message : String(err)));
    }
    return;
  }
  try {
    const found = await invoke<string | null>("find_game", { preferred: path || null });
    if (found) {
      draftPath = found;
      cfgGameText.textContent = shortPath(draftPath);
      syncStoreBtn();
    } else {
      draftPath = "";
      cfgGameText.textContent = shortPath("");
    }
  } catch (err) {
    flashStatus("err", shortErr(err instanceof Error ? err.message : String(err)));
  }
});

btnStore.addEventListener("click", async (e) => {
  e.preventDefault();
  e.stopPropagation();
  try {
    await invoke("open_game_store");
    flashStatus("ok", "Store ouvert — reviens après l’install");
  } catch (err) {
    flashStatus("err", shortErr(err instanceof Error ? err.message : String(err)));
  }
});

function persistDraft(lobby?: {
  name: string;
  mapId: number;
  gameMode: number;
  levelId: number;
}) {
  const L = lobby ?? readLobbyDraft();
  config = {
    ...config,
    gamePath: draftPath,
    lastLobbyName: L.name,
    lastMapId: L.mapId,
    lastGameMode: L.gameMode,
  };
  saveConfig(config);
  cfgGameText.textContent = shortPath(config.gamePath);
}

btnPartyToggle.addEventListener("click", async () => {
  if (busy || booting) return;

  if (partyLive) {
    setStatus("busy", "Arrêt…");
    try {
      await invoke("stop_stub");
      partyLive = false;
      activeParty = null;
      syncPartyToggle();
      flashStatus("ok", "Partie arrêtée");
    } catch (err) {
      flashStatus("err", shortErr(err instanceof Error ? err.message : String(err)));
      await refreshPartyStatus();
    }
    return;
  }

  const lobby = readLobbyDraft();
  persistDraft(lobby);
  syncStoreBtn();
  setStatus("busy", "Création…");
  try {
    await ensurePartyWithDraft(lobby);
    openConfig(false);
    flashStatus(
      "ok",
      `« ${lobby.name} » · ${levelLabel(lobby.gameMode, lobby.mapId)}`,
    );
  } catch (err) {
    flashStatus("err", shortErr(err instanceof Error ? err.message : String(err)));
    await refreshPartyStatus();
  }
});

let statusClearTimer = 0;

function flashStatus(kind: "ok" | "err", message: string, ms = 2200) {
  setStatus(kind, message);
  window.clearTimeout(statusClearTimer);
  statusClearTimer = window.setTimeout(() => {
    if (busy || booting) return;
    setStatus("idle", "");
  }, ms);
}

function shortErr(raw: string): string {
  const msg = raw.replace(/^Error:\s*/i, "").trim();
  if (/node/i.test(msg) && /spawn|not found|ENOENT|PATH/i.test(msg)) {
    return "Installe Node.js puis réessaie";
  }
  if (/permission|access is denied|EACCES|EPERM/i.test(msg)) {
    return "Accès refusé — relance en admin si besoin";
  }
  if (/introuvable|not found|ENOENT/i.test(msg) && /jeu|exe|executable|blitz/i.test(msg)) {
    return "Blitz Brigade introuvable — Config";
  }
  if (/stub/i.test(msg) && /introuvable|manquant|missing/i.test(msg)) {
    return "Stub manquant — réinstalle BB-EMU";
  }
  if (/déjà|already/i.test(msg)) return "Déjà en ligne";
  if (/port/i.test(msg) && /in use|EADDRINUSE|occup/i.test(msg)) {
    return "Une partie est déjà ouverte";
  }
  if (/network|fetch|ENOTFOUND|timed out|timeout|ECONN/i.test(msg)) {
    return "Réseau indisponible";
  }
  if (/signature|pubkey|updater/i.test(msg)) return "MAJ invalide ou corrompue";
  return msg.length > 42 ? `${msg.slice(0, 39)}…` : msg;
}

async function launchGame() {
  if (!config.gamePath && draftPath) {
    config = { ...config, gamePath: draftPath };
    saveConfig(config);
  }
  const lobby = readLobbyDraft();
  persistDraft(lobby);
  setStatus("busy", "Lancement…");
  try {
    await ensurePartyWithDraft(lobby);
    await invoke<string>("launch_game", {
      gamePath: config.gamePath,
      lobbyName: lobby.name,
      levelId: lobby.levelId,
      mapId: lobby.mapId,
      maxPlayers: 8,
      gameMode: lobby.gameMode,
    });
    flashStatus(
      "ok",
      `Jeu lancé · ${levelLabel(lobby.gameMode, lobby.mapId)}`,
    );
  } catch (e) {
    flashStatus("err", shortErr(e instanceof Error ? e.message : String(e)));
    await refreshPartyStatus();
  }
}

btnLaunch.addEventListener("click", () => {
  if (busy || booting) return;
  void launchGame();
});

window.setInterval(() => {
  if (booting || busy) return;
  void refreshPartyStatus();
}, 2000);

async function autoDetectGame() {
  const previous = config.gamePath;
  try {
    const found = await invoke<string | null>("find_game", {
      preferred: previous || null,
    });
    if (found) {
      draftPath = found;
      cfgGameText.textContent = shortPath(found);
      if (found !== previous) {
        config = { ...config, gamePath: found };
        saveConfig(config);
        flashStatus("ok", previous ? "Chemin jeu mis à jour" : "Jeu détecté");
      }
    } else if (previous) {
      config = { ...config, gamePath: "" };
      draftPath = "";
      cfgGameText.textContent = shortPath("");
      saveConfig(config);
    }
  } catch {
    // ignore
  }
  syncStoreBtn();
}

window.addEventListener("focus", () => {
  void autoDetectGame();
});

async function boot() {
  setBoot(8);

  let pct = 8;
  const tick = window.setInterval(() => {
    if (!booting || pct >= 55) return;
    pct += 3;
    bootBarFill.style.width = `${pct}%`;
  }, 120);

  setBoot(Math.max(pct, 18));
  try {
    const update = await check();
    if (update) {
      setBoot(30);
      let downloaded = 0;
      let total = 0;
      await update.downloadAndInstall((event) => {
        switch (event.event) {
          case "Started":
            total = event.data.contentLength ?? 0;
            downloaded = 0;
            setBoot(35);
            break;
          case "Progress":
            downloaded += event.data.chunkLength;
            if (total > 0) {
              setBoot(35 + Math.round((downloaded / total) * 55));
            } else {
              pct = Math.min(88, pct + 2);
              setBoot(pct);
            }
            break;
          case "Finished":
            setBoot(95);
            break;
        }
      });
      window.clearInterval(tick);
      setBoot(100);
      await relaunch();
      return;
    }
  } catch {
    // Pas de serveur MAJ / hors ligne : on continue
  }

  setBoot(Math.max(pct, 62));
  await autoDetectGame();

  setBoot(Math.max(pct, 78));
  window.clearInterval(tick);
  setBoot(100);
  await new Promise((r) => setTimeout(r, 320));
  finishBoot();
}

void boot();
