import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
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
  CircleDot,
} from "lucide";
import type { IconNode } from "lucide";
import "./styles.css";

type Config = {
  port: number;
  lobbyName: string;
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
  gamePath: "",
};
const PORT_MAX_LEN = 5;
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
      port: Number(parsed.port) || DEFAULT_CONFIG.port,
      lobbyName: String(parsed.lobbyName || DEFAULT_CONFIG.lobbyName).slice(0, LOBBY_MAX_LEN),
      gamePath: String(parsed.gamePath || ""),
    };
  } catch {
    return { ...DEFAULT_CONFIG };
  }
}

function saveConfig(cfg: Config) {
  localStorage.setItem("bb-emu-config", JSON.stringify(cfg));
}

function shortPath(p: string): string {
  if (!p) return "Choisir BlitzBrigade.exe…";
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
            <button class="bb-btn bb-duo-play" id="btn-launch" type="button">
              <span id="play-label">PLAY</span>
            </button>
            <button class="bb-btn bb-duo-config" id="btn-config" type="button" aria-label="Configurer">
              <span id="ico-config"></span>
            </button>
          </div>
          <div class="update-panel" id="update-panel" hidden>
            <p class="update-info" id="update-info"></p>
            <div class="update-bar" id="update-bar" hidden aria-hidden="true">
              <div class="update-bar-fill" id="update-bar-fill"></div>
            </div>
          </div>
          <div class="status">
            <span class="status-icon" id="status-dot"></span>
            <span id="status-text">Prêt</span>
          </div>
          <button type="button" class="update-check-btn" id="btn-check-update">
            Vérifier les MAJ
          </button>
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
          <button type="button" class="cfg-path-btn" id="cfg-game-btn" title="Choisir l'exécutable">
            <span id="cfg-game-text"></span>
            <span id="ico-folder"></span>
          </button>
        </div>
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
const statusDot = document.querySelector<HTMLElement>("#status-dot")!;
const statusText = document.querySelector<HTMLElement>("#status-text")!;
const updatePanel = document.querySelector<HTMLElement>("#update-panel")!;
const updateInfo = document.querySelector<HTMLElement>("#update-info")!;
const updateBar = document.querySelector<HTMLElement>("#update-bar")!;
const updateBarFill = document.querySelector<HTMLElement>("#update-bar-fill")!;

document.querySelector("#win-min")!.addEventListener("click", () => win.minimize());
document.querySelector("#win-close")!.addEventListener("click", () => win.close());

let config = loadConfig();
let draftPath = config.gamePath;
let stubOnline = false;
let busy = false;

cfgPort.value = String(config.port).slice(0, PORT_MAX_LEN);
cfgLobby.value = config.lobbyName.slice(0, LOBBY_MAX_LEN);
cfgGameText.textContent = shortPath(draftPath);

cfgPort.addEventListener("input", () => {
  cfgPort.value = cfgPort.value.replace(/\D/g, "").slice(0, PORT_MAX_LEN);
});
cfgLobby.addEventListener("input", () => {
  cfgLobby.value = cfgLobby.value.slice(0, LOBBY_MAX_LEN);
});

function setStatus(kind: "idle" | "ok" | "err" | "busy", message: string) {
  statusDot.classList.remove("ok", "err", "busy");
  const iconMap = {
    idle: CircleDot,
    ok: CircleCheck,
    err: CircleAlert,
    busy: LoaderCircle,
  } as const;
  if (kind !== "idle") statusDot.classList.add(kind);
  mountIcon(statusDot, iconMap[kind], 16);
  const svg = statusDot.querySelector("svg");
  if (svg) {
    svg.setAttribute("stroke-width", "2.25");
    svg.setAttribute("stroke-linecap", "round");
    svg.setAttribute("stroke-linejoin", "round");
  }
  statusText.textContent = message;
  busy = kind === "busy";
  btnLaunch.disabled = busy;
  playLabel.textContent = stubOnline ? "STOP" : "PLAY";
}

function isConfigOpen() {
  return configEl.classList.contains("open");
}

function openConfig(open: boolean) {
  if (open) {
    cfgPort.value = String(config.port);
    cfgLobby.value = config.lobbyName;
    draftPath = config.gamePath;
    cfgGameText.textContent = shortPath(draftPath);
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
  try {
    const selected = await open({
      multiple: false,
      directory: false,
      filters: [{ name: "Exécutable", extensions: ["exe"] }],
      title: "Choisir Blitz Brigade",
    });
    if (typeof selected === "string" && selected) {
      draftPath = selected;
      cfgGameText.textContent = shortPath(draftPath);
    }
  } catch (err) {
    setStatus("err", err instanceof Error ? err.message : String(err));
  }
});

btnSave.addEventListener("click", () => {
  config = {
    port: Math.min(65535, Math.max(1, Number(cfgPort.value) || 7891)),
    lobbyName: (cfgLobby.value || "StubLobby").slice(0, LOBBY_MAX_LEN),
    gamePath: draftPath,
  };
  cfgPort.value = String(config.port);
  cfgLobby.value = config.lobbyName;
  cfgGameText.textContent = shortPath(config.gamePath);
  saveConfig(config);
  setStatus(stubOnline ? "ok" : "idle", stubOnline ? "En ligne" : "Config enregistrée");
  openConfig(false);
});

function shortErr(raw: string): string {
  const msg = raw.replace(/^Error:\s*/i, "").trim();
  if (/node/i.test(msg) && /spawn|not found|ENOENT/i.test(msg)) {
    return "Node.js introuvable";
  }
  if (/introuvable|not found|ENOENT/i.test(msg) && /jeu|exe|executable/i.test(msg)) {
    return "Jeu introuvable";
  }
  if (/stub/i.test(msg) && /introuvable|manquant/i.test(msg)) {
    return "Stub introuvable";
  }
  if (/déjà/i.test(msg)) return "Déjà en ligne";
  if (/port/i.test(msg) && /in use|EADDRINUSE/i.test(msg)) return "Port déjà utilisé";
  return msg.length > 48 ? `${msg.slice(0, 45)}…` : msg;
}

async function refreshStatus() {
  try {
    const st = await invoke<StubStatus>("get_status");
    stubOnline = st.running;
    if (!st.stub_ready) {
      setStatus("err", "Stub introuvable");
      return;
    }
    if (st.running) setStatus("ok", "En ligne");
    else setStatus("idle", "Arrêté");
  } catch (e) {
    setStatus("err", shortErr(e instanceof Error ? e.message : String(e)));
  }
}

btnLaunch.addEventListener("click", async () => {
  if (busy) return;

  if (stubOnline) {
    setStatus("busy", "Arrêt…");
    try {
      await invoke<string>("stop_stub");
      stubOnline = false;
      setStatus("idle", "Arrêté");
    } catch (e) {
      setStatus("err", shortErr(e instanceof Error ? e.message : String(e)));
    }
    return;
  }

  if (!config.gamePath) {
    setStatus("err", "Choisis le jeu dans Config");
    openConfig(true);
    return;
  }

  setStatus("busy", "Lancement…");
  try {
    const msg = await invoke<string>("launch_all", {
      port: config.port,
      lobbyName: config.lobbyName,
      gamePath: config.gamePath,
    });
    stubOnline = true;
    if (msg.includes("En ligne")) setStatus("ok", "En ligne");
    else if (/échec|introuvable/i.test(msg)) setStatus("err", shortErr(msg));
    else setStatus("ok", shortErr(msg));
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    if (msg.includes("déjà")) {
      stubOnline = true;
      setStatus("ok", "En ligne");
    } else {
      setStatus("err", shortErr(msg));
    }
  }
});

function hideUpdatePanel() {
  updatePanel.hidden = true;
  updateBar.hidden = true;
  updateBar.setAttribute("aria-hidden", "true");
  updateBarFill.style.width = "0%";
  updateInfo.textContent = "";
}

function setUpdateProgress(pct: number) {
  updateBar.hidden = false;
  updateBar.setAttribute("aria-hidden", "false");
  updateBarFill.style.width = `${Math.max(0, Math.min(100, pct))}%`;
}

async function runUpdater(manual = false) {
  updatePanel.hidden = false;
  updateBar.hidden = true;
  updateBar.setAttribute("aria-hidden", "true");
  updateBarFill.style.width = "0%";
  updateInfo.textContent = "Recherche de mises à jour…";

  try {
    const update = await check();
    if (!update) {
      if (manual) {
        updateInfo.textContent = "Déjà à jour";
        window.setTimeout(() => hideUpdatePanel(), 1800);
      } else {
        hideUpdatePanel();
      }
      return;
    }

    const notes = (update.body || "").trim();
    updateInfo.textContent = notes
      ? `Mise à jour ${update.version} — ${notes}`
      : `Mise à jour ${update.version} disponible`;

    let downloaded = 0;
    let total = 0;

    await update.downloadAndInstall((event) => {
      switch (event.event) {
        case "Started":
          total = event.data.contentLength ?? 0;
          downloaded = 0;
          updateInfo.textContent = `Téléchargement ${update.version}…`;
          setUpdateProgress(total ? 0 : 8);
          break;
        case "Progress":
          downloaded += event.data.chunkLength;
          if (total > 0) {
            const pct = Math.round((downloaded / total) * 100);
            setUpdateProgress(pct);
            updateInfo.textContent = `Téléchargement ${update.version} — ${pct}%`;
          } else {
            setUpdateProgress(Math.min(90, (Number.parseFloat(updateBarFill.style.width) || 0) + 4));
          }
          break;
        case "Finished":
          setUpdateProgress(100);
          updateInfo.textContent = "Installation…";
          break;
      }
    });

    updateInfo.textContent = "Redémarrage…";
    await relaunch();
  } catch (e) {
    if (manual) {
      updatePanel.hidden = false;
      updateBar.hidden = true;
      updateInfo.textContent = shortErr(e instanceof Error ? e.message : String(e));
      window.setTimeout(() => hideUpdatePanel(), 2800);
    } else {
      hideUpdatePanel();
    }
  }
}

document.querySelector("#btn-check-update")!.addEventListener("click", () => {
  void runUpdater(true);
});

refreshStatus();
setInterval(() => {
  if (!busy) refreshStatus();
}, 3000);

void runUpdater(false);
