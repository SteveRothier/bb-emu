/**
 * UDP stub for Blitz Brigade local MP (port 7891 by default).
 *
 * Join:  0x01→0x02 ; 0x03→0x05+0x16+0x10
 * Keepalive after 0x11: 0x06 ReqClock → (0x07) → 0x08 TimeSync → 0x12 StartGame
 * ACK: on flag≠0 inbound, emit 0x0F with peerB/seqMask (piggybacked on all replies too)
 * Ping: every 3s after join, 0x0D + 0x0E
 */

const dgram = require("dgram");
const {
  parsePacket,
  parseClientDetails,
  parseSendClock,
  typeName,
  buildServerGameDetails,
  buildLobbyDetails,
  buildNewPlayer,
  buildStartLoading,
  buildStartGame,
  buildReqClock,
  buildTimeSync,
  buildCommsPing,
  buildPlayerPings,
  buildAckOnly,
  buildPositionsBundle,
  parsePlayerTransform,
  createAckState,
  noteReliableSeq,
  ackHeaderFields,
  GAME_ID,
} = require("./packet");

const PORT = Number(process.env.BB_MP_PORT || 7891);
const HOST = process.env.BB_MP_HOST || "0.0.0.0";
const LOBBY_NAME = process.env.BB_LOBBY_NAME || "StubLobby";
const PING_MS = Number(process.env.BB_MP_PING_MS || 3000);
const CLOCK_FALLBACK_MS = Number(process.env.BB_MP_CLOCK_FALLBACK_MS || 500);

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

function sendDiscoverReply(sock, rinfo, peer) {
  send(
    sock,
    buildServerGameDetails({
      magic: GAME_ID,
      serverName: LOBBY_NAME.length >= 2 ? LOBBY_NAME : "St",
      playerCount: 1,
      deviceName: "stub-host",
      headerOpts: headerOpts(peer),
    }),
    rinfo,
    "0x02 ServerGameDetails"
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
      descriptor: Buffer.from([0x00, 0x01, 0x00, 0x00]),
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
  console.log(`lobby="${LOBBY_NAME}" gameId=0x${GAME_ID.toString(16)} ping=${PING_MS}ms`);
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
    // Echo a minimal server-style bundle so a future client can see host positions.
    if (peer.started || peer.joined) {
      const trMsg = msgs.find((m) => m.type === 0x17);
      const tr = trMsg ? parsePlayerTransform(trMsg.body) : null;
      const env = msgs.find((m) => m.type === 0x4d);
      const timeMs = env && env.body.length >= 4 ? env.body.readUInt32BE(0) : Date.now() & 0xffffffff;
      send(
        sock,
        buildPositionsBundle({
          timeMs,
          playerId: tr ? tr.playerId : 0,
          qx: tr ? tr.qx : 0,
          qy: tr ? tr.qy : 0,
          qz: tr ? tr.qz : 0,
          yawByte: tr ? tr.yawByte : 0,
          pitchByte: tr ? tr.pitchByte : 0,
          headerOpts: headerOpts(peer),
        }),
        rinfo,
        "0x4D+0x17 PositionsEcho"
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
