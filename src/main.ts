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
  port: number;
  lobbyName: string;
  lobbies: string[];
  gamePath: string;
};

type StubStatus = {
  running: boolean;
  stub_ready: boolean;
  stub_path: string | null;
};

const DEFAULT_CONFIG: Config = {
  port: 7891,
  lobbyName: "StubLobby",
  lobbies: ["StubLobby", "Public", "Friends"],
  gamePath: "",
};
const PORT_MAX_LEN = 5;
const LOBBY_MAX_LEN = 16;
const win = getCurrentWindow();

function normalizeLobbies(list: unknown, current: string): string[] {
  const raw = Array.isArray(list) ? list.map(String) : [];
  const cleaned = raw
    .map((s) => s.trim().slice(0, LOBBY_MAX_LEN))
    .filter(Boolean);
  if (current && !cleaned.includes(current)) cleaned.unshift(current);
  if (!cleaned.length) cleaned.push(...DEFAULT_CONFIG.lobbies);
  return [...new Set(cleaned)].slice(0, 12);
}

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
    if (!raw) return { ...DEFAULT_CONFIG, lobbies: [...DEFAULT_CONFIG.lobbies] };
    const parsed = JSON.parse(raw) as Partial<Config>;
    const lobbyName = String(parsed.lobbyName || DEFAULT_CONFIG.lobbyName).slice(0, LOBBY_MAX_LEN);
    return {
      port: Number(parsed.port) || DEFAULT_CONFIG.port,
      lobbyName,
      lobbies: normalizeLobbies(parsed.lobbies, lobbyName),
      gamePath: String(parsed.gamePath || ""),
    };
  } catch {
    return { ...DEFAULT_CONFIG, lobbies: [...DEFAULT_CONFIG.lobbies] };
  }
}

function saveConfig(cfg: Config) {
  localStorage.setItem("bb-emu-config", JSON.stringify(cfg));
}

function isStorePath(p: string): boolean {
  const s = p.toLowerCase();
  return s.includes("windowsapps") && (s.includes("gameloftsa.blitzbrigade") || s.includes("infamousheroesentry.exe"));
}

function shortPath(p: string): string {
  if (!p || isStorePath(p)) return "Blitz Brigade (Store)";
  const parts = p.replace(/\//g, "\\").split("\\");
  if (parts.length <= 2) return p;
  return `…\\${parts.slice(-2).join("\\")}`;
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
          <div class="actions bb-duo">
            <button class="bb-btn bb-duo-play" id="btn-launch" type="button" disabled>
              <span id="play-label">PLAY</span>
            </button>
            <button class="bb-btn bb-duo-config" id="btn-config" type="button" aria-label="Configurer">
              <span id="ico-config"></span>
            </button>
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
        <label class="cfg-band">
          <span class="cfg-band-label">Port</span>
          <input id="cfg-port" type="text" inputmode="numeric" maxlength="5" autocomplete="off" />
        </label>
        <label class="cfg-band">
          <span class="cfg-band-label">Lobby</span>
          <input id="cfg-lobby" type="text" maxlength="16" autocomplete="off" />
        </label>
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
        <div class="config-actions">
          <button class="bb-btn bb-tan bb-sm" id="btn-save" type="button">
            <span>OK</span>
          </button>
          <button class="bb-btn bb-tan bb-sm" id="btn-close" type="button">
            <span>Annuler</span>
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
const playLabel = document.querySelector<HTMLElement>("#play-label")!;
const btnConfig = document.querySelector<HTMLButtonElement>("#btn-config")!;
const btnSave = document.querySelector<HTMLButtonElement>("#btn-save")!;
const btnClose = document.querySelector<HTMLButtonElement>("#btn-close")!;
const configEl = document.querySelector<HTMLElement>("#config")!;
const cfgPort = document.querySelector<HTMLInputElement>("#cfg-port")!;
const cfgLobby = document.querySelector<HTMLInputElement>("#cfg-lobby")!;
const cfgGameBtn = document.querySelector<HTMLButtonElement>("#cfg-game-btn")!;
const cfgGameText = document.querySelector<HTMLElement>("#cfg-game-text")!;
const btnStore = document.querySelector<HTMLButtonElement>("#btn-store")!;
const statusRow = document.querySelector<HTMLElement>("#status-row")!;
const statusDot = document.querySelector<HTMLElement>("#status-dot")!;
const statusText = document.querySelector<HTMLElement>("#status-text")!;
const bootPanel = document.querySelector<HTMLElement>("#boot-panel")!;
const bootBarFill = document.querySelector<HTMLElement>("#boot-bar-fill")!;
const appVersionEl = document.querySelector<HTMLElement>("#app-version")!;

document.querySelector("#win-min")!.addEventListener("click", () => win.minimize());
document.querySelector("#win-close")!.addEventListener("click", () => win.close());

let config = loadConfig();
let draftPath = config.gamePath;
let stubOnline = false;
let busy = false;

cfgPort.value = String(config.port).slice(0, PORT_MAX_LEN);
cfgLobby.value = config.lobbyName.slice(0, LOBBY_MAX_LEN);
cfgGameText.textContent = shortPath(draftPath);

void getVersion()
  .then((v) => {
    appVersionEl.textContent = `v${v}`;
  })
  .catch(() => {
    appVersionEl.textContent = "v?";
  });

cfgPort.addEventListener("input", () => {
  cfgPort.value = cfgPort.value.replace(/\D/g, "").slice(0, PORT_MAX_LEN);
});
cfgLobby.addEventListener("input", () => {
  cfgLobby.value = cfgLobby.value.slice(0, LOBBY_MAX_LEN);
});

let booting = true;

function hasGameInstalled() {
  return true;
}

function syncPlayBtn() {
  playLabel.textContent = stubOnline ? "STOP" : "PLAY";
  // STOP toujours possible ; PLAY seulement si le jeu est installé
  const blockPlay = !stubOnline && !hasGameInstalled();
  btnLaunch.disabled = booting || busy || blockPlay;
  btnLaunch.title = blockPlay
    ? "Installe le jeu pour lancer"
    : stubOnline
      ? "Arrêter"
      : "Lancer";
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
}

function setStatus(kind: "idle" | "ok" | "err" | "busy", message: string) {
  if (booting) return;
  // Idle / prêt : rien sous les boutons
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
    cfgPort.value = String(config.port);
    cfgLobby.value = config.lobbyName;
    draftPath = config.gamePath;
    cfgGameText.textContent = shortPath(draftPath);
    syncStoreBtn();
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

btnSave.addEventListener("click", () => {
  const lobbyName = (cfgLobby.value.trim() || "StubLobby").slice(0, LOBBY_MAX_LEN);
  config = {
    port: Math.min(65535, Math.max(1, Number(cfgPort.value) || 7891)),
    lobbyName,
    lobbies: normalizeLobbies([...config.lobbies, lobbyName], lobbyName),
    gamePath: draftPath,
  };
  cfgPort.value = String(config.port);
  cfgLobby.value = config.lobbyName;
  cfgGameText.textContent = shortPath(config.gamePath);
  saveConfig(config);
  syncStoreBtn();
  openConfig(false);
  if (stubOnline) setStatus("ok", `En ligne — ${config.lobbyName}`);
  else flashStatus("ok", "Config enregistrée");
});

let statusClearTimer = 0;

function flashStatus(kind: "ok" | "err", message: string, ms = 2200) {
  setStatus(kind, message);
  window.clearTimeout(statusClearTimer);
  statusClearTimer = window.setTimeout(() => {
    if (!busy && !booting) setStatus("idle", "");
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
    return "Port occupé — change-le dans Config";
  }
  if (/network|fetch|ENOTFOUND|timed out|timeout|ECONN/i.test(msg)) {
    return "Réseau indisponible";
  }
  if (/signature|pubkey|updater/i.test(msg)) return "MAJ invalide ou corrompue";
  return msg.length > 42 ? `${msg.slice(0, 39)}…` : msg;
}

async function refreshStatus() {
  try {
    const st = await invoke<StubStatus>("get_status");
    stubOnline = st.running;
    if (!st.stub_ready) {
      setStatus("err", "Stub manquant — réinstalle BB-EMU");
      return;
    }
    if (st.running) setStatus("ok", `En ligne — ${config.lobbyName}`);
    else setStatus("idle", "");
  } catch (e) {
    flashStatus("err", shortErr(e instanceof Error ? e.message : String(e)));
  }
}

btnLaunch.addEventListener("click", async () => {
  if (busy || booting) return;

  if (stubOnline) {
    setStatus("busy", "Arrêt…");
    try {
      await invoke<string>("stop_stub");
      stubOnline = false;
      setStatus("idle", "");
    } catch (e) {
      flashStatus("err", shortErr(e instanceof Error ? e.message : String(e)));
    }
    return;
  }

  if (!config.gamePath && draftPath) {
    config = { ...config, gamePath: draftPath };
    saveConfig(config);
  }

  setStatus("busy", "Lancement…");
  try {
    const msg = await invoke<string>("launch_all", {
      port: config.port,
      lobbyName: config.lobbyName,
      gamePath: config.gamePath,
    });
    stubOnline = true;
    if (msg.includes("En ligne")) setStatus("ok", `En ligne — ${config.lobbyName}`);
    else if (/échec|introuvable/i.test(msg)) flashStatus("err", shortErr(msg));
    else setStatus("ok", shortErr(msg) || `En ligne — ${config.lobbyName}`);
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    if (msg.includes("déjà")) {
      stubOnline = true;
      setStatus("ok", `En ligne — ${config.lobbyName}`);
    } else {
      flashStatus("err", shortErr(msg));
    }
  }
});

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
  try {
    const st = await invoke<StubStatus>("get_status");
    stubOnline = st.running;
  } catch {
    // ignore
  }

  window.clearInterval(tick);
  setBoot(100);
  await new Promise((r) => setTimeout(r, 320));
  finishBoot();
  if (stubOnline) setStatus("ok", `En ligne — ${config.lobbyName}`);
}

setInterval(() => {
  if (!busy && !booting) refreshStatus();
}, 3000);

void boot();
