/**
 * UDP stub for Blitz Brigade local MP (port 7891 by default).
 *
 * Join:  0x01→0x02 ; 0x03→0x05+0x16+0x10
 * Keepalive after 0x11: 0x06 ReqClock → (0x07) → 0x08 TimeSync → 0x12 StartGame
 * ACK: on flag≠0 inbound, emit 0x0F with peerB/seqMask (piggybacked on all replies too)
 * Ping: every 3s after join, 0x0D + 0x0E
 *
 * CLI (prioritaire sur env):
 *   --port N --lobby NAME --map ID --max-players N --mode N --level N --log PATH
 */

const fs = require("fs");
const dgram = require("dgram");
const {
  parsePacket,
  parseClientDetails,
  parseSendClock,
  typeName,
  buildServerGameDetails,
  buildLobbyDetails,
  buildLobbyDescriptor,
  buildNewPlayer,
  buildStartLoading,
  buildStartGame,
  buildReqClock,
  buildTimeSync,
  buildCommsPing,
  buildPlayerPings,
  buildAckOnly,
  buildPositionsEnvelope,
  parsePlayerTransform,
  createAckState,
  noteReliableSeq,
  ackHeaderFields,
  GAME_ID,
} = require("./packet");
const { resolveModeMap } = require("./levels");

function parseArgs(argv) {
  const out = {};
  for (let i = 2; i < argv.length; i++) {
    const a = argv[i];
    if (!a.startsWith("--")) continue;
    const key = a.slice(2);
    const next = argv[i + 1];
    if (next != null && !next.startsWith("--")) {
      out[key] = next;
      i++;
    } else {
      out[key] = true;
    }
  }
  return out;
}

function clampU8(n, fallback) {
  const v = Number(n);
  if (!Number.isFinite(v)) return fallback;
  return Math.max(0, Math.min(255, v | 0));
}

const args = parseArgs(process.argv);

const PORT = Number(args.port || process.env.BB_MP_PORT || 7891);
const HOST = process.env.BB_MP_HOST || "0.0.0.0";
const LOBBY_NAME = String(args.lobby || process.env.BB_LOBBY_NAME || "MaPartie")
  .trim()
  .slice(0, 16) || "MaPartie";
const MAX_PLAYERS = clampU8(args["max-players"] ?? process.env.BB_MAX_PLAYERS, 8) || 8;
const LOBBY = resolveModeMap({
  mode: args.mode ?? process.env.BB_GAME_MODE,
  map: args.map ?? process.env.BB_MAP_ID,
});
const MAP_ID = LOBBY.mapId;
const GAME_MODE = LOBBY.modeId;
const PING_MS = Number(process.env.BB_MP_PING_MS || 3000);
const CLOCK_FALLBACK_MS = Number(process.env.BB_MP_CLOCK_FALLBACK_MS || 500);

if (args.log) {
  try {
    const logPath = String(args.log);
    const stream = fs.createWriteStream(logPath, { flags: "a" });
    const wrap = (fn) => (chunk) => {
      try {
        stream.write(chunk);
      } catch (_) {
        /* ignore */
      }
      fn(chunk);
    };
    process.stdout.write = wrap(process.stdout.write.bind(process.stdout));
    process.stderr.write = wrap(process.stderr.write.bind(process.stderr));
  } catch (e) {
    console.error("log file open failed", e.message);
  }
}

const peers = new Map();

function peerKey(rinfo) {
  return `${rinfo.address}:${rinfo.port}`;
}

function getPeer(rinfo) {
  const k = peerKey(rinfo);
  if (!peers.has(k)) {
    peers.set(k, {
      addr: rinfo,
      ack: createAckState(),
      details: null,
      discoverReplied: false,
      joined: false,
      clockStarted: false,
      clockDone: false,
      started: false,
      clockTimer: null,
      pingSeq: 0,
    });
  }
  return peers.get(k);
}

function noteInboundHeader(peer, header) {
  if (!header || header.flag === 0) return false;
  noteReliableSeq(peer.ack, header.peerA);
  return true;
}

function headerOpts(peer) {
  const { peerB, seqMask } = ackHeaderFields(peer.ack);
  return { flag: 0, peerA: 0, peerB, seqMask };
}

function send(sock, buf, rinfo, label) {
  sock.send(buf, rinfo.port, rinfo.address, (err) => {
    if (err) console.error(`[send ${label}]`, err.message);
    else console.log(`→ ${rinfo.address}:${rinfo.port} ${label} (${buf.length} B)`);
  });
}

function sendAck(sock, rinfo, peer) {
  send(sock, buildAckOnly(headerOpts(peer)), rinfo, "0x0F AckOnly");
}

function playerFromDetails(details, deviceId = 0) {
  return {
    name: details?.playerName || "Player",
    appearance: Buffer.from([0, 0, 0]),
    deviceId,
  };
}

function connectedCount() {
  let n = 1; // host slot
  for (const p of peers.values()) {
    if (p.joined) n++;
  }
  return Math.min(n, MAX_PLAYERS);
}

function sendDiscoverReply(sock, rinfo, peer) {
  send(
    sock,
    buildServerGameDetails({
      magic: GAME_ID,
      gameMode: GAME_MODE,
      mapId: MAP_ID,
      playerCount: connectedCount(),
      maxPlayers: MAX_PLAYERS,
      hostName: LOBBY_NAME,
      headerOpts: headerOpts(peer),
    }),
    rinfo,
    `0x02 ServerGameDetails ${LOBBY.label}`
  );
  peer.discoverReplied = true;
}

function sendJoinSequence(sock, rinfo, peer) {
  if (peer.joined) return;
  const player = playerFromDetails(peer.details, 0);
  const opts = headerOpts(peer);

  send(
    sock,
    buildLobbyDetails({
      seq: 1,
      descriptor: buildLobbyDescriptor({
        gameMode: GAME_MODE,
        mapId: MAP_ID,
        maxPlayers: MAX_PLAYERS,
        flags: 0,
      }),
      players: [player],
      headerOpts: opts,
    }),
    rinfo,
    "0x05 LobbyDetails"
  );
  send(
    sock,
    buildNewPlayer({
      playerId: 0,
      name: player.name,
      headerOpts: opts,
    }),
    rinfo,
    "0x16 NewPlayer"
  );
  send(
    sock,
    buildStartLoading({ playerId: 0, headerOpts: opts }),
    rinfo,
    "0x10 StartLoading"
  );
  peer.joined = true;
}

function finishClockAndStart(sock, rinfo, peer, clientTime) {
  if (peer.started) return;
  if (peer.clockTimer) {
    clearTimeout(peer.clockTimer);
    peer.clockTimer = null;
  }
  const opts = headerOpts(peer);
  const delayVal = clientTime != null ? clientTime >>> 0 : 0;
  send(sock, buildTimeSync({ rtt: 100, delayVal, headerOpts: opts }), rinfo, "0x08 TimeSync");
  send(
    sock,
    buildStartGame({
      countdownMs: 2000,
      timeDelta: 0,
      syncToken: 0,
      headerOpts: opts,
    }),
    rinfo,
    "0x12 StartGame"
  );
  peer.clockDone = true;
  peer.started = true;
}

function startClockSync(sock, rinfo, peer) {
  if (peer.clockStarted || peer.started) return;
  peer.clockStarted = true;
  send(sock, buildReqClock({ round: 0, headerOpts: headerOpts(peer) }), rinfo, "0x06 ReqClock");
  peer.clockTimer = setTimeout(() => {
    console.log(`  clock fallback (no 0x07) for ${peerKey(rinfo)}`);
    finishClockAndStart(sock, rinfo, peer, 0);
  }, CLOCK_FALLBACK_MS);
}

function sendPing(sock, rinfo, peer) {
  if (!peer.joined) return;
  const opts = headerOpts(peer);
  peer.pingSeq = (peer.pingSeq + 1) >>> 0;
  send(
    sock,
    buildCommsPing({ pingSeq: peer.pingSeq, slotPing: 50, headerOpts: opts }),
    rinfo,
    "0x0D CommsPing"
  );
  send(
    sock,
    buildPlayerPings({ entries: [{ slot: 0, ping: 50 }], headerOpts: opts }),
    rinfo,
    "0x0E PlayerPings"
  );
}

const sock = dgram.createSocket("udp4");

sock.on("listening", () => {
  const a = sock.address();
  console.log(`bb-mp-stub listening UDP ${a.address}:${a.port}`);
  console.log(
    `lobby="${LOBBY_NAME}" ${LOBBY.label} map=${MAP_ID} mode=${GAME_MODE} ` +
      `max=${MAX_PLAYERS} gameId=0x${GAME_ID.toString(16)} ping=${PING_MS}ms`
  );
});

sock.on("message", (msg, rinfo) => {
  const peer = getPeer(rinfo);
  const parsed = parsePacket(msg);

  if (!parsed.ok) {
    console.log(`← ${peerKey(rinfo)} invalid (${parsed.error}) len=${msg.length}`);
    return;
  }

  const reliable = noteInboundHeader(peer, parsed.header);
  console.log(
    `← ${peerKey(rinfo)} type=0x${parsed.type.toString(16)} (${typeName(parsed.type)}) ` +
      `flag=${parsed.header.flag} seq=${parsed.header.peerA} payload=${parsed.payload.length}B`
  );

  if (reliable) {
    sendAck(sock, rinfo, peer);
  }

  if (parsed.type === 0x01) {
    sendDiscoverReply(sock, rinfo, peer);
    return;
  }

  if (parsed.type === 0x03) {
    const details = parseClientDetails(parsed.body);
    if (details) {
      peer.details = details;
      console.log(
        `  ClientDetails name="${details.playerName}" device="${details.deviceNameStr}" ` +
          `magic=0x${details.magic.toString(16)}`
      );
    } else {
      peer.details = { playerName: "Player" };
    }
    sendJoinSequence(sock, rinfo, peer);
    return;
  }

  if (parsed.type === 0x07) {
    const sc = parseSendClock(parsed.body);
    console.log(
      `  SendClock round=${sc ? sc.round : "?"} time=${sc ? sc.clientTime : "?"}`
    );
    if (peer.clockStarted && !peer.started) {
      finishClockAndStart(sock, rinfo, peer, sc ? sc.clientTime : 0);
    }
    return;
  }

  if (parsed.type === 0x11) {
    console.log(`  FinishedLoading from ${peerKey(rinfo)}`);
    if (!peer.joined) sendJoinSequence(sock, rinfo, peer);
    startClockSync(sock, rinfo, peer);
    return;
  }

  if (parsed.type === 0x14) {
    // Client → host : demande d'état à l'entrée en jeu (réponse via inject).
    console.log(`  SyncRequest seq=${parsed.header.peerA}`);
    return;
  }

  if (parsed.type === 0x61) {
    // Client → host : choix équipe/classe — body = id, faction, class.
    const id = parsed.body[0] || 0;
    const faction = parsed.body[1] || 0;
    const classId = parsed.body[2] || 0;
    peer.classSelect = { id, faction, classId };
    console.log(
      `  ClassSelect id=${id} faction=${faction} class=${classId} seq=${parsed.header.peerA}`
    );
    return;
  }

  if (parsed.type === 0x0f) {
    return;
  }

  // Positions tick: 0x4D envelope (+ optional 0x17 transforms in same datagram)
  const msgs = parsed.messages || [];
  const hasPos = msgs.some((m) => m.type === 0x4d || m.type === 0x17);
  if (hasPos) {
    for (const m of msgs) {
      if (m.type === 0x4d && m.body.length >= 4) {
        const t = m.body.readUInt32BE(0);
        console.log(`  PositionsEnvelope timeMs=${t}`);
      } else if (m.type === 0x17) {
        const tr = parsePlayerTransform(m.body);
        if (tr) {
          console.log(
            `  PlayerTransform id=${tr.playerId} q=(${tr.qx},${tr.qy},${tr.qz}) ` +
              `yaw=${tr.yawByte} pitch=${tr.pitchByte}`
          );
        }
      }
    }
    // Echo 0x4D only — NEVER echo 0x17 with the client's pitchByte.
    // Replaying pitch=199 locked the local camera looking at the floor.
    if (peer.started || peer.joined) {
      const env = msgs.find((m) => m.type === 0x4d);
      const timeMs =
        env && env.body.length >= 4
          ? env.body.readUInt32BE(0)
          : Date.now() & 0xffffffff;
      send(
        sock,
        buildPositionsEnvelope({
          timeMs,
          headerOpts: headerOpts(peer),
        }),
        rinfo,
        "0x4D PositionsEcho"
      );
    }
    return;
  }

  if (!peer.discoverReplied && !peer.joined) {
    sendDiscoverReply(sock, rinfo, peer);
  }
});

const pingTimer = setInterval(() => {
  for (const peer of peers.values()) {
    if (peer.joined) sendPing(sock, peer.addr, peer);
  }
}, PING_MS);

sock.on("error", (err) => {
  console.error("socket error", err);
  clearInterval(pingTimer);
  sock.close();
});

sock.bind(PORT, HOST);

process.on("SIGINT", () => {
  console.log("\nshutting down");
  clearInterval(pingTimer);
  for (const peer of peers.values()) {
    if (peer.clockTimer) clearTimeout(peer.clockTimer);
  }
  sock.close();
  process.exit(0);
});
