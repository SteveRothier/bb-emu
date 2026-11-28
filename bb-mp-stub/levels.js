/**
 * Cartes / modes pour le 0x02 et le descripteur 0x05.
 *
 * 0x02 nameBytes (L=4), host stamp name[1]=playerCount ;
 * Joueurs UI = name[1]/name[2] (sinon mapId à [2] → « 1/0 ») :
 *   [0] loadIdx | [1] playerCount | [2] maxPlayers | [3] loadIdx
 * name[0]=name[3]=load (GetMPMapName = table 33). apply_mp / 0x10 = load.
 * Mode stream = streamMode ?? modeId (TDM Store → FFA, sans restore mid-class).
 *
 * Matrice maps↔modes : maps-modes.json (partagé avec le launcher).
 */

const data = require("./maps-modes.json");

const MAPS = data.maps;
const MODES = data.modes;
const MAPS_BY_MODE = Object.fromEntries(
  Object.entries(data.mapsByMode).map(([k, v]) => [Number(k), v]),
);
/** Plan table 33 par (mode, map) — source : maps-modes.json levelPlans. */
const LEVEL_PLANS = Object.fromEntries(
  Object.entries(data.levelPlans || {}).map(([mode, maps]) => [
    Number(mode),
    Object.fromEntries(
      Object.entries(maps).map(([map, plan]) => [
        Number(map),
        {
          titleIdx: plan.title,
          loadIdx: plan.load,
          aliasFrom:
            plan.aliasFrom === undefined || plan.aliasFrom === null
              ? null
              : plan.aliasFrom,
          streamMode:
            plan.streamMode === undefined || plan.streamMode === null
              ? null
              : plan.streamMode,
        },
      ]),
    ),
  ]),
);

function clampU8(n, fallback) {
  const v = Number(n);
  if (!Number.isFinite(v)) return fallback;
  return Math.max(0, Math.min(255, v | 0));
}

function mapsForMode(modeId) {
  const ids = MAPS_BY_MODE[clampU8(modeId, 0)] || MAPS_BY_MODE[0];
  return ids.map((id) => MAPS.find((m) => m.id === id)).filter(Boolean);
}

/**
 * Index carte dans mapsByMode[mode] pour le wire 0x02 name[3].
 * Le client (liste serveurs) lit un **index de mode**, pas l'id absolu
 * (sinon map=5 en Chacun pour soi → 5%4=1 → « Ruines »).
 */
function wireMapIndex(modeId, mapId) {
  const allowed = MAPS_BY_MODE[clampU8(modeId, 0)] || MAPS_BY_MODE[0];
  const abs = clampU8(mapId, allowed[0]);
  const idx = allowed.indexOf(abs);
  return idx >= 0 ? idx : 0;
}

function absoluteMapId(modeId, wireIndex) {
  const allowed = MAPS_BY_MODE[clampU8(modeId, 0)] || MAPS_BY_MODE[0];
  const i = clampU8(wireIndex, 0);
  return allowed[Math.min(i, allowed.length - 1)] ?? allowed[0];
}

/**
 * Plan niveau — cellule (mode, map) de maps-modes.json levelPlans.
 * titleIdx = logs / lobby-live seulement (hors wire).
 * loadIdx = name[0]=name[3] / menu+0x58 / MapIdx / 0x10 / sid.
 * aliasFrom = donor table pour assets (launcher only) ; null = natif.
 * streamMode = GameMode au load si ≠ modeId (ex. TDM→3).
 */
function levelPlan(modeId, mapId) {
  const mode = clampU8(modeId, 0);
  const map = clampU8(mapId, 0);
  const cell = LEVEL_PLANS[mode] && LEVEL_PLANS[mode][map];
  if (cell) {
    return {
      titleIdx: cell.titleIdx,
      loadIdx: cell.loadIdx,
      aliasFrom: cell.aliasFrom,
      streamMode: cell.streamMode,
    };
  }
  // Hors matrice (ex. Mad id 1) — ne pas inventer un load CTF.
  return { titleIdx: map, loadIdx: map, aliasFrom: null, streamMode: null };
}

function levelStreamMode(modeId, mapId) {
  const plan = levelPlan(modeId, mapId);
  return plan.streamMode == null ? clampU8(modeId, 0) : plan.streamMode;
}

function levelTitleIdx(modeId, mapId) {
  return levelPlan(modeId, mapId).titleIdx;
}

function levelLoadIdx(modeId, mapId) {
  return levelPlan(modeId, mapId).loadIdx;
}

/** Index table load (MapIdx / 0x10) — pas le titre liste. */
function retailToLevelIdx(modeId, mapId) {
  return levelLoadIdx(modeId, mapId);
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
    wireMapIndex: wireMapIndex(modeId, mapId),
    modeLabel: modeMeta?.label ?? String(modeId),
    mapLabel: mapMeta?.label ?? String(mapId),
    label: `${modeMeta?.label ?? modeId} · ${mapMeta?.label ?? mapId}`,
  };
}

/** @deprecated alias — plus d’index de niveau dense pour le 0x02 */
function resolveLevelIndex(opts = {}) {
  const r = resolveModeMap(opts);
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
  wireMapIndex,
  absoluteMapId,
  levelPlan,
  levelTitleIdx,
  levelLoadIdx,
  levelStreamMode,
  retailToLevelIdx,
};
