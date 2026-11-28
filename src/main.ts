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
import mapsModes from "../bb-mp-stub/maps-modes.json";
import "./styles.css";

type Config = {
  gamePath: string;
  lastLobbyName: string;
  lastMapId: number;
  lastGameMode: number;
  /** Multiplicateur sensibilité souris (1 = défaut packer 0.06). */
  lookSensitivity: number;
};

const DEFAULT_CONFIG: Config = {
  gamePath: "",
  lastLobbyName: "MaPartie",
  lastMapId: 0, // Cathédrale (wire)
  lastGameMode: 0, // Domination
  lookSensitivity: 1,
};

/** Source unique : bb-mp-stub/maps-modes.json (aussi lu par levels.js). */
const MAP_OPTIONS = mapsModes.maps;
const MODE_OPTIONS = mapsModes.modes;
const MAPS_BY_MODE: Record<number, number[]> = Object.fromEntries(
  Object.entries(mapsModes.mapsByMode).map(([k, v]) => [Number(k), v as number[]]),
);

/** Modes autorisés pour une carte (dérivé de mapsByMode). */
const MODES_BY_MAP: Record<number, number[]> = (() => {
  const out: Record<number, number[]> = {};
  for (const m of MAP_OPTIONS) out[m.id] = [];
  for (const [modeStr, maps] of Object.entries(MAPS_BY_MODE)) {
    const modeId = Number(modeStr);
    for (const mapId of maps) {
      if (!out[mapId].includes(modeId)) out[mapId].push(modeId);
    }
  }
  for (const id of Object.keys(out)) out[Number(id)].sort((a, b) => a - b);
  return out;
})();

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
    const sens = Number(parsed.lookSensitivity);
    return {
      gamePath: String(parsed.gamePath || ""),
      lastLobbyName: String(parsed.lastLobbyName || DEFAULT_CONFIG.lastLobbyName).slice(
        0,
        LOBBY_MAX_LEN,
      ),
      lastMapId: Math.max(0, Math.min(255, Number(parsed.lastMapId) || 0)),
      lastGameMode: Math.max(0, Math.min(255, Number(parsed.lastGameMode) || 0)),
      lookSensitivity: Number.isFinite(sens)
        ? Math.max(0.25, Math.min(3, sens))
        : DEFAULT_CONFIG.lookSensitivity,
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

        <div class="bb-lobby-select">
          <div class="bb-cycle" data-cycle="mode">
            <p class="bb-cycle-label">Mode de jeu</p>
            <div class="bb-cycle-row">
              <button type="button" class="bb-cycle-arrow" id="cfg-mode-prev" aria-label="Mode précédent">
                <span class="bb-cycle-chevron bb-cycle-chevron-l" aria-hidden="true"></span>
              </button>
              <div class="bb-cycle-panel" id="cfg-mode-panel" role="status" aria-live="polite">
                <span id="cfg-mode-text">Domination</span>
              </div>
              <button type="button" class="bb-cycle-arrow" id="cfg-mode-next" aria-label="Mode suivant">
                <span class="bb-cycle-chevron bb-cycle-chevron-r" aria-hidden="true"></span>
              </button>
            </div>
          </div>

          <div class="bb-cycle" data-cycle="map">
            <p class="bb-cycle-label">Carte</p>
            <div class="bb-cycle-row">
              <button type="button" class="bb-cycle-arrow" id="cfg-map-prev" aria-label="Carte précédente">
                <span class="bb-cycle-chevron bb-cycle-chevron-l" aria-hidden="true"></span>
              </button>
              <div class="bb-cycle-panel" id="cfg-map-panel" role="status" aria-live="polite">
                <span id="cfg-map-text">Cathédrale</span>
              </div>
              <button type="button" class="bb-cycle-arrow" id="cfg-map-next" aria-label="Carte suivante">
                <span class="bb-cycle-chevron bb-cycle-chevron-r" aria-hidden="true"></span>
              </button>
            </div>
          </div>
        </div>

        <p class="cfg-kicker">Souris FPS</p>
        <label class="cfg-band cfg-band-sens">
          <span class="cfg-band-label">Sens.</span>
          <input
            id="cfg-look-sens"
            type="range"
            min="25"
            max="300"
            step="5"
            aria-label="Sensibilité souris"
          />
          <span class="cfg-sens-val" id="cfg-look-sens-val">100%</span>
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
const cfgModeText = document.querySelector<HTMLElement>("#cfg-mode-text")!;
const cfgMapText = document.querySelector<HTMLElement>("#cfg-map-text")!;
const cfgModePrev = document.querySelector<HTMLButtonElement>("#cfg-mode-prev")!;
const cfgModeNext = document.querySelector<HTMLButtonElement>("#cfg-mode-next")!;
const cfgMapPrev = document.querySelector<HTMLButtonElement>("#cfg-map-prev")!;
const cfgMapNext = document.querySelector<HTMLButtonElement>("#cfg-map-next")!;
const cfgLookSens = document.querySelector<HTMLInputElement>("#cfg-look-sens")!;
const cfgLookSensVal = document.querySelector<HTMLElement>("#cfg-look-sens-val")!;
const cfgGameBtn = document.querySelector<HTMLButtonElement>("#cfg-game-btn")!;

/** Sélection courante mode/carte (wire ids). */
let draftModeId = 0;
let draftMapId = 0;
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
  if (same) {
    await syncLobbyInjectQuiet();
    return;
  }

  if (partyLive) {
    // Hot-update sans kill Node (évite course Quais→Fort pendant Rejoindre).
    const msg = await invoke<string>("update_party_lobby", {
      port: 7891,
      lobbyName: lobby.name,
      levelId: lobby.levelId,
      mapId: lobby.mapId,
      maxPlayers: 8,
      gameMode: lobby.gameMode,
    });
    if (typeof msg === "string" && msg.includes("frozen")) {
      // Join en cours : brouillon UI ok, carte wire figée.
      return;
    }
    activeParty = { ...lobby };
    return;
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

/** Applique le brouillon au stub si une partie tourne déjà (changement flèche). */
async function pushDraftIfPartyLive() {
  if (!partyLive || busy || booting) return;
  const lobby = readLobbyDraft();
  persistDraft(lobby);
  try {
    await ensurePartyWithDraft(lobby);
  } catch (err) {
    flashStatus("err", shortErr(err instanceof Error ? err.message : String(err)));
    await refreshPartyStatus();
  }
}

async function syncLobbyInjectQuiet() {
  try {
    await invoke("sync_lobby_inject");
  } catch {
    // jeu pas encore patché / pas lancé — ignore
  }
}

function syncNameCount() {
  cfgNameCount.textContent = `${cfgName.value.length}/${LOBBY_MAX_LEN}`;
}

function mapsAllowedForMode(modeId: number): number[] {
  return MAPS_BY_MODE[modeId] || MAPS_BY_MODE[0];
}

function modesAllowedForMap(mapId: number): number[] {
  return MODES_BY_MAP[mapId] || MODE_OPTIONS.map((m) => m.id);
}

function clampMapForMode(modeId: number, mapId: number): number {
  const allowed = mapsAllowedForMode(modeId);
  return allowed.includes(mapId) ? mapId : allowed[0];
}

function clampModeForMap(mapId: number, modeId: number): number {
  const allowed = modesAllowedForMap(mapId);
  return allowed.includes(modeId) ? modeId : allowed[0];
}

function renderModeCycle() {
  const mode = MODE_OPTIONS.find((m) => m.id === draftModeId) ?? MODE_OPTIONS[0];
  cfgModeText.textContent = mode.label.toUpperCase();
}

function renderMapCycle() {
  draftMapId = clampMapForMode(draftModeId, draftMapId);
  const map = MAP_OPTIONS.find((m) => m.id === draftMapId) ?? MAP_OPTIONS[0];
  cfgMapText.textContent = map.label.toUpperCase();
}

function cycleMode(delta: number) {
  const ids = MODE_OPTIONS.map((m) => m.id);
  const i = Math.max(0, ids.indexOf(draftModeId));
  draftModeId = ids[(i + delta + ids.length) % ids.length];
  draftMapId = clampMapForMode(draftModeId, draftMapId);
  renderModeCycle();
  renderMapCycle();
  void pushDraftIfPartyLive();
}

function cycleMap(delta: number) {
  const allowed = mapsAllowedForMode(draftModeId);
  const i = Math.max(0, allowed.indexOf(draftMapId));
  draftMapId = allowed[(i + delta + allowed.length) % allowed.length];
  // Sécurité : mode toujours valide pour la carte affichée.
  draftModeId = clampModeForMap(draftMapId, draftModeId);
  renderModeCycle();
  renderMapCycle();
  void pushDraftIfPartyLive();
}

cfgModePrev.addEventListener("click", () => cycleMode(-1));
cfgModeNext.addEventListener("click", () => cycleMode(1));
cfgMapPrev.addEventListener("click", () => cycleMap(-1));
cfgMapNext.addEventListener("click", () => cycleMap(1));

function fillDraftFromConfig() {
  cfgName.value = (config.lastLobbyName || DEFAULT_CONFIG.lastLobbyName).slice(0, LOBBY_MAX_LEN);
  draftModeId = MODE_OPTIONS.some((m) => m.id === config.lastGameMode)
    ? config.lastGameMode
    : 0;
  draftMapId = clampMapForMode(draftModeId, config.lastMapId ?? 0);
  renderModeCycle();
  renderMapCycle();
  const pct = Math.round((config.lookSensitivity || 1) * 100);
  cfgLookSens.value = String(Math.max(25, Math.min(300, pct)));
  cfgLookSensVal.textContent = `${cfgLookSens.value}%`;
  syncNameCount();
}

async function pushLookSettings() {
  const sensitivity = Math.max(0.25, Math.min(3, Number(cfgLookSens.value) / 100 || 1));
  config = { ...config, lookSensitivity: sensitivity };
  saveConfig(config);
  try {
    await invoke("set_look_settings", { sensitivity });
  } catch {
    /* ignore — watcher peut être hors Windows */
  }
}

function readLobbyDraft() {
  let name = cfgName.value.trim() || "MaPartie";
  name = [...name].slice(0, LOBBY_MAX_LEN).join("");
  const gameMode = draftModeId;
  const mapId = clampMapForMode(gameMode, draftMapId);
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
fillDraftFromConfig();
void pushLookSettings();

cfgLookSens.addEventListener("input", () => {
  cfgLookSensVal.textContent = `${cfgLookSens.value}%`;
});
cfgLookSens.addEventListener("change", () => {
  void pushLookSettings();
});

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
    lookSensitivity: Math.max(0.25, Math.min(3, Number(cfgLookSens.value) / 100 || 1)),
  };
  saveConfig(config);
  cfgGameText.textContent = shortPath(config.gamePath);
  void pushLookSettings();
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
