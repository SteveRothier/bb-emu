/**
 * Blitz Brigade UDPPacket codec (InfamousHeroesEntry RE).
 * Wire: [13-byte header][payload...]
 * Fixed types: payload = [type][fields...]
 * VAR types: payload = [type][u16 BE totalLen][fields...]
 *   totalLen = 1 + 2 + fields.length  (type + len field + body)
 */

const HEADER_SIZE = 13;
const GAME_ID = 0x0ab25571;

/** Size table DAT_00c3a280 — 0xFFFFFF means VAR (extrait utile + 0x14/0x61). */
const SIZE_TABLE = {
  0x01: 5,
  0x02: 0xffffff,
  0x03: 0xffffff,
  0x05: 0xffffff,
  0x06: 2,
  0x07: 6,
  0x08: 9,
  0x0b: 1,
  0x0d: 9,
  0x0e: 0xffffff,
  0x0f: 1,
  0x10: 9,
  0x11: 1,
  0x12: 13,
  0x13: 14,
  0x14: 1,
  0x15: 0xffffff,
  0x16: 30,
  0x17: 10,
  0x25: 0xffffff,
  0x4d: 5,
  0x61: 4,
  0x62: 0xffffff,
  0x63: 0xffffff,
  0x6d: 5,
  0x78: 217,
  0x80: 0xffffff,
};

function u16be(n) {
  const b = Buffer.alloc(2);
  b.writeUInt16BE(n & 0xffff, 0);
  return b;
}

function u32be(n) {
  const b = Buffer.alloc(4);
  b.writeUInt32BE(n >>> 0, 0);
  return b;
}

function padName20(name) {
  const out = Buffer.alloc(0x14);
  const src = Buffer.from(String(name || ""), "utf8").subarray(0, 0x13);
  src.copy(out);
  return out;
}

/**
 * Header: flag | seqOut u32 LE | ackMax u32 LE | ackMask u32 LE
 * flag=0 skips seq/dedup on recv (FUN_0074e370).
 */
function buildHeader({ flag = 0, peerA = 0, peerB = 0, seqMask = 0 } = {}) {
  const h = Buffer.alloc(HEADER_SIZE);
  h[0] = flag & 0xff;
  h.writeUInt32LE(peerA >>> 0, 1);
  h.writeUInt32LE(peerB >>> 0, 5);
  h.writeUInt32LE(seqMask >>> 0, 9);
  return h;
}

function wrapPacket(payload, headerOpts) {
  return Buffer.concat([buildHeader(headerOpts), payload]);
}

/** VAR wrapper: type + u16 BE (type+len+body) + body */
function wrapVar(type, body, headerOpts) {
  const totalLen = 1 + 2 + body.length;
  const payload = Buffer.concat([Buffer.from([type & 0xff]), u16be(totalLen), body]);
  return wrapPacket(payload, headerOpts);
}

/**
 * 0x01 Discover — fixed 5: type + u32be gameId
 * Real broadcast uses zero header; we still wrap for local tests.
 */
function buildDiscover({ magic = GAME_ID } = {}) {
  return wrapPacket(Buffer.concat([Buffer.from([0x01]), u32be(magic)]), { flag: 0 });
}

/**
 * 0x02 ServerGameDetails (VAR) — host → client on Discover.
 * FUN_0074fd80 / liste UI (FUN_0074dbf0):
 *   body: u32be gameId | u8 L + nameBytes | deviceName[0x14]
 * nameBytes (L=4) — host stamp name[1]=playerCount ;
 * Joueurs UI = name[1]/name[2] :
 *   [0] loadIdx | [1] playerCount | [2] maxPlayers | [3] loadIdx
 * name[0]=name[3]=load (GetMPMapName = table 33). apply_mp / 0x10 = load.
 * deviceName[0x14] = nom d'hôte (« Comms: Discovered Server name %s »).
 */
function buildServerGameDetails({
  magic = GAME_ID,
  gameMode = 0,
  mapId = 0,
  playerCount = 1,
  maxPlayers = 8,
  // legacy ignored
  levelIndex,
  hostName = "MaPartie",
  serverName,
  deviceName,
  headerOpts,
} = {}) {
  const host = hostName || deviceName || serverName || "MaPartie";
  void levelIndex;
  const slots = maxPlayers < 2 ? 8 : maxPlayers & 0xff;
  // Params historiques gameMode/mapId = octets name[0]/name[3] (souvent levelIdx).
  const nameBytes = Buffer.from([
    gameMode & 0xff,
    playerCount & 0xff,
    slots,
    mapId & 0xff,
  ]);
  const body = Buffer.concat([
    u32be(magic),
    Buffer.from([nameBytes.length]),
    nameBytes,
    padName20(host),
  ]);
  return wrapVar(0x02, body, headerOpts);
}

/**
 * Descriptor opaque du 0x05 (stocké lobby+0x30 / len+0x34, setter FUN_0068ec70).
 * Champs UI liés: Infamous_GameMode, Infamous_MapIdx, Infamous_MaxPlayers.
 * Layout: [gameMode, mapId, maxPlayers, flags] — mode/carte du niveau MP choisi.
 */
function buildLobbyDescriptor({
  gameMode = 0,
  mapId = 0,
  maxPlayers = 8,
  flags = 0,
} = {}) {
  return Buffer.from([
    gameMode & 0xff,
    mapId & 0xff,
    maxPlayers & 0xff,
    flags & 0xff,
  ]);
}

/**
 * 0x03 ClientSendDetails (VAR) — client → host on Connect.
 * body: u32be gameId | u8 L + playerName | deviceName[0x14]
 */
function buildClientDetails({
  magic = GAME_ID,
  playerName = "Player",
  deviceName = "client",
  headerOpts,
} = {}) {
  const nameBuf = Buffer.from(playerName, "utf8");
  if (nameBuf.length > 255) throw new Error("player name too long");
  const body = Buffer.concat([
    u32be(magic),
    Buffer.from([nameBuf.length]),
    nameBuf,
    padName20(deviceName),
  ]);
  return wrapVar(0x03, body, headerOpts || { flag: 1 });
}

function parseClientDetails(body) {
  if (!body || body.length < 4 + 1 + 0x14) return null;
  const magic = body.readUInt32BE(0);
  const nameLen = body[4];
  if (body.length < 5 + nameLen + 0x14) return null;
  const playerName = body.subarray(5, 5 + nameLen).toString("utf8");
  const deviceName = body.subarray(5 + nameLen, 5 + nameLen + 0x14);
  return { magic, playerName, deviceName, deviceNameStr: cString(deviceName) };
}

function cString(buf) {
  const z = buf.indexOf(0);
  return buf.subarray(0, z < 0 ? buf.length : z).toString("utf8");
}

/**
 * 0x05 LobbyDetails (VAR) — MP_ServerSendLocalLobbyDetails (FUN_00690060).
 * body: u32be seq | u8 descLen + descriptor | u8 N |
 *   N × { name[0x14], u8 apLen + appearance, u8 deviceId }
 * Player slot stride in host memory: 0x40 (name@0, apPtr@0x14, apLen@0x18, deviceId@0x1c).
 */
function buildLobbyDetails({
  seq = 1,
  descriptor = buildLobbyDescriptor({}),
  players = [],
  headerOpts,
} = {}) {
  const desc = Buffer.isBuffer(descriptor) ? descriptor : Buffer.from(descriptor);
  if (desc.length > 255) throw new Error("descriptor too long");
  const parts = [u32be(seq), Buffer.from([desc.length]), desc, Buffer.from([players.length & 0xff])];
  for (const p of players) {
    const name = padName20(p.name || "P");
    const ap = Buffer.isBuffer(p.appearance)
      ? p.appearance
      : Buffer.from(p.appearance || [0, 0, 0]);
    if (ap.length > 255) throw new Error("appearance too long");
    parts.push(name, Buffer.from([ap.length]), ap, Buffer.from([(p.deviceId ?? 0) & 0xff]));
  }
  return wrapVar(0x05, Buffer.concat(parts), headerOpts);
}

/** Type 0x11 — FinishedLoading (fixe=1). */
function buildFinishedLoading(headerOpts) {
  return wrapPacket(Buffer.from([0x11]), headerOpts || { flag: 0 });
}

/**
 * Type 0x10 — StartLoading (fixe=9).
 * u8 playerId, u8 b0 (+0x3100), u32be word (+0x3104), u8 b1 (+0x30fc), u8 b2 (+0x3110)
 */
function buildStartLoading({
  playerId = 0,
  b0 = 0,
  word = 0,
  b1 = 0,
  b2 = 0,
  headerOpts,
} = {}) {
  const payload = Buffer.concat([
    Buffer.from([0x10, playerId & 0xff, b0 & 0xff]),
    u32be(word),
    Buffer.from([b1 & 0xff, b2 & 0xff]),
  ]);
  if (payload.length !== 9) throw new Error("0x10 size");
  return wrapPacket(payload, headerOpts);
}

/**
 * Type 0x12 — StartGame (fixe=13): 3× u32be.
 */
function buildStartGame({ countdownMs = 2000, timeDelta = 0, syncToken = 0, headerOpts } = {}) {
  const payload = Buffer.concat([
    Buffer.from([0x12]),
    u32be(countdownMs),
    u32be(timeDelta),
    u32be(syncToken),
  ]);
  if (payload.length !== 13) throw new Error("0x12 size");
  return wrapPacket(payload, headerOpts);
}

/**
 * Type 0x16 — NewPlayer (fixe=30 = type + 29).
 * playerId, used(1), deviceId, team, class, name[0x14], u32be token
 */
function buildNewPlayer({
  playerId = 0,
  used = 1,
  deviceId = 0,
  team = 0,
  classId = 0,
  name = "Player",
  token = 0,
  headerOpts,
} = {}) {
  const payload = Buffer.concat([
    Buffer.from([
      0x16,
      playerId & 0xff,
      used & 0xff,
      deviceId & 0xff,
      team & 0xff,
      classId & 0xff,
    ]),
    padName20(name),
    u32be(token),
  ]);
  if (payload.length !== 30) throw new Error(`0x16 size ${payload.length}`);
  return wrapPacket(payload, headerOpts);
}

/** 0x06 ReqClock (fixe=2): type + u8 roundIndex. Server→client, flag=0. */
function buildReqClock({ round = 0, headerOpts } = {}) {
  return wrapPacket(Buffer.from([0x06, round & 0xff]), headerOpts);
}

/** 0x07 SendClock (fixe=6): type + u8 round + u32be clientTime. Client→server. */
function buildSendClock({ round = 0, clientTime = 0, headerOpts } = {}) {
  const payload = Buffer.concat([
    Buffer.from([0x07, round & 0xff]),
    u32be(clientTime),
  ]);
  if (payload.length !== 6) throw new Error("0x07 size");
  return wrapPacket(payload, headerOpts);
}

function parseSendClock(body) {
  if (!body || body.length < 5) return null;
  return { round: body[0], clientTime: body.readUInt32BE(1) };
}

/**
 * 0x08 TimeSync (fixe=9): type + u32be rtt + u32be delayVal.
 * Server→client after clock rounds; flag=1 on real host (stub uses flag=0).
 */
function buildTimeSync({ rtt = 100, delayVal = 0, headerOpts } = {}) {
  const payload = Buffer.concat([Buffer.from([0x08]), u32be(rtt), u32be(delayVal)]);
  if (payload.length !== 9) throw new Error("0x08 size");
  return wrapPacket(payload, headerOpts);
}

/** 0x0D Comms ping (fixe=9): type + u32be pingSeq + u32be slotPing. flag=0. */
function buildCommsPing({ pingSeq = 0, slotPing = 50, headerOpts } = {}) {
  const payload = Buffer.concat([Buffer.from([0x0d]), u32be(pingSeq), u32be(slotPing)]);
  if (payload.length !== 9) throw new Error("0x0d size");
  return wrapPacket(payload, headerOpts);
}

/**
 * 0x0E MP_ServerSendPings (VAR): u8 N + N×{u8 slot, u8 ping}.
 */
function buildPlayerPings({ entries = [{ slot: 0, ping: 50 }], headerOpts } = {}) {
  const parts = [Buffer.from([entries.length & 0xff])];
  for (const e of entries) {
    parts.push(Buffer.from([(e.slot ?? 0) & 0xff, (e.ping ?? 50) & 0xff]));
  }
  return wrapVar(0x0e, Buffer.concat(parts), headerOpts);
}

/** 0x0F SendMessageAcknowledge — type only; ACK lives in header peerB/seqMask. */
function buildAckOnly(headerOpts) {
  return wrapPacket(Buffer.from([0x0f]), headerOpts || { flag: 0 });
}

/**
 * 0x4D Positions envelope (fixe=5): type + u32be timeMs.
 * Real client/server then append sibling msgs (esp. 0x17) in the same datagram.
 */
function buildPositionsEnvelope({ timeMs = 0, headerOpts } = {}) {
  const payload = Buffer.concat([Buffer.from([0x4d]), u32be(timeMs)]);
  if (payload.length !== 5) throw new Error("0x4d size");
  return wrapPacket(payload, headerOpts);
}

/**
 * 0x17 PlayerTransform (fixe=10) — FUN_00693760.
 * u8 playerId, u16be qx, u16be qy, u16be qz, u8 yawByte, u8 pitchByte
 * (quantized floats relative to world origin / scale on host).
 */
function buildPlayerTransform({
  playerId = 0,
  qx = 0,
  qy = 0,
  qz = 0,
  yawByte = 0,
  pitchByte = 0,
  headerOpts,
} = {}) {
  const payload = Buffer.alloc(10);
  payload[0] = 0x17;
  payload[1] = playerId & 0xff;
  payload.writeUInt16BE(qx & 0xffff, 2);
  payload.writeUInt16BE(qy & 0xffff, 4);
  payload.writeUInt16BE(qz & 0xffff, 6);
  payload[8] = yawByte & 0xff;
  payload[9] = pitchByte & 0xff;
  return wrapPacket(payload, headerOpts);
}

function parsePlayerTransform(body) {
  if (!body || body.length < 9) return null;
  return {
    playerId: body[0],
    qx: body.readUInt16BE(1),
    qy: body.readUInt16BE(3),
    qz: body.readUInt16BE(5),
    yawByte: body[7],
    pitchByte: body[8],
  };
}

/** Bundle 0x4D + 0x17 into one datagram (server→client style). */
function buildPositionsBundle({
  timeMs = 0,
  playerId = 0,
  qx = 0,
  qy = 0,
  qz = 0,
  yawByte = 0,
  pitchByte = 0,
  headerOpts,
} = {}) {
  const env = Buffer.concat([Buffer.from([0x4d]), u32be(timeMs)]);
  const tr = Buffer.alloc(10);
  tr[0] = 0x17;
  tr[1] = playerId & 0xff;
  tr.writeUInt16BE(qx & 0xffff, 2);
  tr.writeUInt16BE(qy & 0xffff, 4);
  tr.writeUInt16BE(qz & 0xffff, 6);
  tr[8] = yawByte & 0xff;
  tr[9] = pitchByte & 0xff;
  return wrapPacket(Buffer.concat([env, tr]), headerOpts);
}

/** Message length in payload bytes (including type), from SIZE_TABLE. */
function messageByteLength(payload, offset) {
  if (offset >= payload.length) return 0;
  const type = payload[offset];
  const table = SIZE_TABLE[type];
  if (table === undefined) return 0;
  if (table === 0xffffff) {
    if (offset + 3 > payload.length) return 0;
    const total = payload.readUInt16BE(offset + 1);
    return total > 0 ? total : 0;
  }
  return table;
}

/**
 * Split payload into sub-messages (FUN_00690540 style).
 * Returns [{ type, body, offset, length }, ...]
 */
function parseMessages(payload) {
  const out = [];
  let off = 0;
  while (off < payload.length) {
    const type = payload[off];
    const len = messageByteLength(payload, off);
    if (!len || off + len > payload.length) break;
    const table = SIZE_TABLE[type];
    let body;
    if (table === 0xffffff) body = payload.subarray(off + 3, off + len);
    else body = payload.subarray(off + 1, off + len);
    out.push({ type, body, offset: off, length: len, tableSize: table });
    off += len;
  }
  return out;
}

/**
 * Track reliable inbound sequences for header ACK (FUN_007509e0 style).
 * peerB = max seq; bit i set if (peerB - i) was received (i in 0..31).
 */
function createAckState() {
  return { max: 0, seen: new Set(), hasReliable: false };
}

function noteReliableSeq(state, seq) {
  const s = seq >>> 0;
  if (!state.hasReliable) {
    state.hasReliable = true;
    state.max = s;
  } else if (s > state.max) {
    state.max = s;
  }
  state.seen.add(s);
  for (const x of [...state.seen]) {
    if (state.max - x > 31) state.seen.delete(x);
  }
}

function ackHeaderFields(state) {
  if (!state.hasReliable) return { peerB: 0, seqMask: 0 };
  const peerB = state.max >>> 0;
  let seqMask = 0;
  for (let i = 0; i < 32; i++) {
    if (state.seen.has((peerB - i) >>> 0)) seqMask |= 1 << i;
  }
  return { peerB, seqMask: seqMask >>> 0 };
}

function parsePacket(buf) {
  if (!buf || buf.length < HEADER_SIZE + 1) {
    return { ok: false, error: "too short", length: buf?.length ?? 0 };
  }
  const header = {
    flag: buf[0],
    peerA: buf.readUInt32LE(1),
    peerB: buf.readUInt32LE(5),
    seqMask: buf.readUInt32LE(9),
  };
  const payload = buf.subarray(HEADER_SIZE);
  const type = payload[0];
  const table = SIZE_TABLE[type];
  let body = payload.subarray(1);
  let declaredLen = null;
  if (table === 0xffffff) {
    if (payload.length < 3) return { ok: false, error: "VAR missing length", type, header };
    declaredLen = payload.readUInt16BE(1);
    body = payload.subarray(3);
  }
  const messages = parseMessages(payload);
  return {
    ok: true,
    header,
    type,
    tableSize: table,
    declaredLen,
    body,
    payload,
    messages,
    raw: buf,
  };
}

function typeName(t) {
  const names = {
    0x01: "Discover",
    0x02: "ServerGameDetails",
    0x03: "ClientSendDetails",
    0x05: "LobbyDetails",
    0x06: "ReqClock",
    0x07: "SendClock",
    0x08: "TimeSync",
    0x0d: "CommsPing",
    0x0e: "PlayerPings",
    0x0f: "AckOnly",
    0x10: "StartLoading",
    0x11: "FinishedLoading",
    0x12: "StartGame",
    0x13: "ScoreUpdate",
    0x14: "SyncRequest",
    0x15: "MatchState",
    0x16: "NewPlayer",
    0x17: "PlayerTransform",
    0x25: "Respawn",
    0x4d: "PositionsEnvelope",
    0x61: "ClassSelect",
    0x62: "ClassConfirm",
    0x6d: "MatchStart",
    0x78: "MatchBlob",
  };
  return names[t] || `Type_0x${t.toString(16)}`;
}

module.exports = {
  HEADER_SIZE,
  SIZE_TABLE,
  GAME_ID,
  buildHeader,
  wrapPacket,
  wrapVar,
  padName20,
  buildDiscover,
  buildServerGameDetails,
  buildLobbyDescriptor,
  buildClientDetails,
  parseClientDetails,
  buildLobbyDetails,
  buildFinishedLoading,
  buildStartLoading,
  buildStartGame,
  buildNewPlayer,
  buildReqClock,
  buildSendClock,
  parseSendClock,
  buildTimeSync,
  buildCommsPing,
  buildPlayerPings,
  buildAckOnly,
  buildPositionsEnvelope,
  buildPlayerTransform,
  parsePlayerTransform,
  buildPositionsBundle,
  messageByteLength,
  parseMessages,
  createAckState,
  noteReliableSeq,
  ackHeaderFields,
  parsePacket,
  typeName,
  cString,
};
