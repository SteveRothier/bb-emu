/**
 * Cartes / modes wire pour le 0x02 et le descripteur 0x05.
 *
 * 0x02 nameBytes (L=4), host stamp name[1]=playerCount ;
 * Joueurs UI = name[1]/name[2] (sinon mapId à [2] → « 1/0 ») :
 *   [0] gameMode | [1] playerCount | [2] maxPlayers | [3] mapId
 *
 * Preuves in-game :
 *   - ancien [1,1,8] (L=3) → préfixe « Capture du drapeau » ⇒ mode wire 1 = CTF
 *   - map wire 0 = Cathédrale ; map wire 5 = OVNI
 *     (ordre ci-dessous : Cat … Fort, UFO en dernier)
 */

const MAPS = [
  { id: 0, label: "Cathédrale" },
  { id: 1, label: "Madagascar" },
  { id: 2, label: "Ruines de Madagascar" },
  { id: 3, label: "Quais de Malte" },
  { id: 4, label: "Fort de Malte" },
  { id: 5, label: "Crash OVNI" },
];

/** Dom=0, CTF=1, DM=2, FFA=3 (CTF avant Deathmatch — calibrage wire). */
const MODES = [
  { id: 0, label: "Domination" },
  { id: 1, label: "Capture de drapeau" },
  { id: 2, label: "Match à mort" },
  { id: 3, label: "Chacun pour soi" },
];

/** Cartes autorisées par mode (ids MAPS, ordre menu). */
const MAPS_BY_MODE = {
  0: [0, 1, 2, 4, 5], // Domination — pas Quais
  1: [0, 1, 2, 3, 5], // CTF — pas Fort
  2: [0, 2, 3, 4, 5], // Match à mort — pas Madagascar
  3: [0, 2, 3, 5], // FFA
};

function clampU8(n, fallback) {
  const v = Number(n);
  if (!Number.isFinite(v)) return fallback;
  return Math.max(0, Math.min(255, v | 0));
}

function mapsForMode(modeId) {
  const ids = MAPS_BY_MODE[clampU8(modeId, 0)] || MAPS_BY_MODE[0];
  return ids.map((id) => MAPS.find((m) => m.id === id)).filter(Boolean);
}

function resolveModeMap({ mode, map } = {}) {
  let modeId = clampU8(mode, 0);
  if (!MODES.some((m) => m.id === modeId)) modeId = 0;
  const allowed = MAPS_BY_MODE[modeId] || MAPS_BY_MODE[0];
  let mapId = clampU8(map, allowed[0]);
  if (!allowed.includes(mapId)) mapId = allowed[0];
  const modeMeta = MODES.find((m) => m.id === modeId);
  const mapMeta = MAPS.find((m) => m.id === mapId);
  return {
    modeId,
    mapId,
    modeLabel: modeMeta?.label ?? String(modeId),
    mapLabel: mapMeta?.label ?? String(mapId),
    label: `${modeMeta?.label ?? modeId} · ${mapMeta?.label ?? mapId}`,
  };
}

/** @deprecated alias — plus d’index de niveau dense pour le 0x02 */
function resolveLevelIndex(opts = {}) {
  const r = resolveModeMap(opts);
  // conserve un « level » stable pour logs / UI : mode*6 + map
  return r.modeId * 6 + r.mapId;
}

function levelByIndex(index) {
  const i = clampU8(index, 0);
  const modeId = Math.min(3, Math.floor(i / 6));
  const mapId = i % 6;
  return resolveModeMap({ mode: modeId, map: mapId });
}

function findLevel(modeId, mapId) {
  const r = resolveModeMap({ mode: modeId, map: mapId });
  return {
    index: r.modeId * 6 + r.mapId,
    modeId: r.modeId,
    mapId: r.mapId,
    modeLabel: r.modeLabel,
    mapLabel: r.mapLabel,
    label: r.label,
  };
}

const LEVELS = [];
for (const mode of MODES) {
  for (const map of mapsForMode(mode.id)) {
    LEVELS.push(findLevel(mode.id, map.id));
  }
}

module.exports = {
  MAPS,
  MODES,
  MAPS_BY_MODE,
  LEVELS,
  findLevel,
  levelByIndex,
  resolveLevelIndex,
  resolveModeMap,
  mapsForMode,
};
