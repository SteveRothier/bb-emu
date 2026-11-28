/**
 * Codec + live UDP: join, ACK on reliable, clock before StartGame, ping.
 */
const dgram = require("dgram");
const { spawn } = require("child_process");
const path = require("path");
const {
  parsePacket,
  parseClientDetails,
  parseSendClock,
  buildDiscover,
  buildClientDetails,
  buildFinishedLoading,
  buildSendClock,
  buildLobbyDetails,
  buildNewPlayer,
  buildStartLoading,
  buildStartGame,
  buildServerGameDetails,
  buildLobbyDescriptor,
  buildReqClock,
  buildTimeSync,
  buildCommsPing,
  buildPlayerPings,
  buildAckOnly,
  buildPositionsEnvelope,
  buildPlayerTransform,
  buildPositionsBundle,
  parsePlayerTransform,
  parseMessages,
  createAckState,
  noteReliableSeq,
  ackHeaderFields,
  HEADER_SIZE,
  GAME_ID,
} = require("./packet");

function assert(cond, msg) {
  if (!cond) throw new Error(msg);
}

// --- unit checks ---
{
  const sg = buildServerGameDetails({
    hostName: "MaPartie",
    gameMode: 0,
    mapId: 1,
    playerCount: 2,
    maxPlayers: 8,
  });
  const p = parsePacket(sg);
  assert(p.ok && p.type === 0x02 && p.declaredLen === p.payload.length, "0x02");
  // body: magic(4) + len(1) + name[4] + device[0x14]
  assert(p.body[4] === 4, "0x02 name len");
  assert(
    p.body[5] === 0 && p.body[6] === 2 && p.body[7] === 8 && p.body[8] === 1,
    "0x02 mode/count/max/map"
  );
  assert(p.body.subarray(9, 9 + 8).toString("utf8") === "MaPartie", "0x02 host device");
}

{
  const { resolveModeMap, MODES, wireMapIndex, retailToLevelIdx } = require("./levels");
  assert(MODES[1].label.includes("drapeau"), "mode 1 = CTF");
  const domRuines = resolveModeMap({ mode: 0, map: 2 });
  assert(domRuines.mapId === 2 && domRuines.modeId === 0, "Dom/Ruines");
  assert(wireMapIndex(3, 3) === 0, "wire Quais@FFA helper");
  assert(wireMapIndex(3, 5) === 1, "wire Crash@FFA helper");
  const { levelTitleIdx, levelLoadIdx, levelPlan } = require("./levels");
  assert(resolveModeMap({ mode: 0, map: 1 }).mapId !== 1, "Madagascar hors Dom");
  assert(resolveModeMap({ mode: 1, map: 1 }).mapId !== 1, "Madagascar hors CTF");
  assert(levelPlan(0, 1).loadIdx === 1 && levelPlan(0, 1).aliasFrom == null, "Mad hors matrice");
  assert(levelTitleIdx(1, 2) === 13 && levelLoadIdx(1, 2) === 12, "Ruines@CTF load 12");
  assert(levelTitleIdx(0, 2) === 3 && levelLoadIdx(0, 2) === 12, "Ruines@Dom load 12");
  assert(resolveModeMap({ mode: 2, map: 2 }).mapId !== 2, "Ruines hors TDM");
  assert(resolveModeMap({ mode: 2, map: 0 }).mapId !== 0, "Cathédrale hors TDM");
  assert(levelTitleIdx(1, 3) === 11 && levelLoadIdx(1, 3) === 11, "Quais@CTF load 11");
  assert(levelTitleIdx(2, 3) === 10 && levelLoadIdx(2, 3) === 10, "Quais@TDM load 10");
  assert(levelPlan(2, 3).streamMode === 3, "Quais@TDM stream FFA");
  assert(levelPlan(2, 4).streamMode === 0, "Fort@TDM stream Dom");
  assert(levelTitleIdx(3, 3) === 10 && levelLoadIdx(3, 3) === 10, "Quais@FFA load 10");
  assert(levelTitleIdx(1, 0) === 1 && levelLoadIdx(1, 0) === 0, "Cathédrale@CTF load 0");
  assert(levelTitleIdx(0, 0) === 0 && levelLoadIdx(0, 0) === 0, "Cathédrale@Dom load 0");
  assert(levelTitleIdx(0, 4) === 5 && levelLoadIdx(0, 4) === 5, "Fort@Dom load 5");
  assert(levelPlan(1, 2).aliasFrom == null, "Ruines@CTF pas d'alias");
  assert(levelPlan(0, 2).aliasFrom == null, "Ruines@Dom pas d'alias");
  assert(wireMapIndex(1, 2) === 1, "wire Ruines@CTF = 1");
  const ruinesLoad = levelLoadIdx(1, 2);
  const sgRuines = buildServerGameDetails({
    gameMode: ruinesLoad,
    mapId: ruinesLoad,
    playerCount: 1,
    maxPlayers: 8,
  });
  const pr = parsePacket(sgRuines);
  assert(
    pr.body[5] === 12 && pr.body[8] === 12,
    "0x02 Ruines@CTF name=[12,1,8,12] load"
  );
  const cathLoad = levelLoadIdx(1, 0);
  const sgCath = buildServerGameDetails({
    gameMode: cathLoad,
    mapId: cathLoad,
    playerCount: 1,
    maxPlayers: 8,
  });
  const pc = parsePacket(sgCath);
  assert(
    pc.body[5] === 0 && pc.body[8] === 0,
    "0x02 Cathédrale@CTF name=[0,1,8,0] load"
  );
  const quaisLoad = levelLoadIdx(2, 3);
  const sgQuais = buildServerGameDetails({
    gameMode: quaisLoad,
    mapId: quaisLoad,
    playerCount: 1,
    maxPlayers: 8,
  });
  const pq = parsePacket(sgQuais);
  assert(pq.body[5] === 10 && pq.body[8] === 10, "0x02 Quais@TDM name=[10,*,*,10] load");
}

{
  const { levelLoadIdx: loadIdx05 } = require("./levels");
  const fortLoad = loadIdx05(0, 4);
  const d = buildLobbyDescriptor({
    gameMode: 0,
    mapId: fortLoad,
    maxPlayers: 8,
    flags: 0,
  });
  assert(d.equals(Buffer.from([0, 5, 8, 0])), "descriptor Fort@Dom load=5");
  const ruinesLoad05 = loadIdx05(1, 2);
  const d2 = buildLobbyDescriptor({
    gameMode: 1,
    mapId: ruinesLoad05,
    maxPlayers: 8,
    flags: 0,
  });
  assert(d2.equals(Buffer.from([1, 12, 8, 0])), "descriptor Ruines load=12");
}

{
  const cd = buildClientDetails({ playerName: "Alice", deviceName: "dev1" });
  const d = parseClientDetails(parsePacket(cd).body);
  assert(d && d.playerName === "Alice", "0x03");
}

{
  const lobby = buildLobbyDetails({
    seq: 1,
    players: [{ name: "Alice", appearance: Buffer.from([0, 0, 0]), deviceId: 0 }],
  });
  assert(parsePacket(lobby).type === 0x05, "0x05");
}

{
  assert(buildNewPlayer({ name: "A" }).length === HEADER_SIZE + 30, "0x16");
  assert(parsePacket(buildReqClock({ round: 3 })).payload.length === 2, "0x06");
  const sc = parsePacket(buildSendClock({ round: 3, clientTime: 0x12345678 }));
  assert(sc.payload.length === 6, "0x07 len");
  const parsed = parseSendClock(sc.body);
  assert(parsed.round === 3 && parsed.clientTime === 0x12345678, "0x07 fields");
  assert(parsePacket(buildTimeSync({})).payload.length === 9, "0x08");
  assert(parsePacket(buildCommsPing({ pingSeq: 1 })).payload.length === 9, "0x0d");
  const pp = parsePacket(buildPlayerPings({ entries: [{ slot: 0, ping: 40 }] }));
  assert(pp.type === 0x0e && pp.body[0] === 1 && pp.body[2] === 40, "0x0e");
  assert(parsePacket(buildAckOnly()).type === 0x0f, "0x0f");
}

{
  assert(parsePacket(buildPositionsEnvelope({ timeMs: 99 })).payload.length === 5, "0x4d");
  const tr = parsePacket(buildPlayerTransform({ playerId: 1, qx: 10, qy: 20, qz: 30, yawByte: 7, pitchByte: 8 }));
  assert(tr.payload.length === 10, "0x17 len");
  const fields = parsePlayerTransform(tr.body);
  assert(fields && fields.playerId === 1 && fields.qx === 10 && fields.qy === 20, "0x17 fields");
  const bundle = parsePacket(
    buildPositionsBundle({ timeMs: 42, playerId: 0, qx: 1, qy: 2, qz: 3, yawByte: 4, pitchByte: 5 })
  );
  assert(bundle.messages.length === 2, `bundle msgs ${bundle.messages.length}`);
  assert(bundle.messages[0].type === 0x4d && bundle.messages[1].type === 0x17, "bundle order");
  assert(parseMessages(bundle.payload).length === 2, "parseMessages");
}

{
  const st = createAckState();
  noteReliableSeq(st, 0);
  noteReliableSeq(st, 2);
  noteReliableSeq(st, 1);
  const h = ackHeaderFields(st);
  assert(h.peerB === 2, `ack max ${h.peerB}`);
  assert((h.seqMask & 1) !== 0 && (h.seqMask & 2) !== 0 && (h.seqMask & 4) !== 0, "ack mask bits");
}

console.log("codec self-checks OK");

const PORT = 17891;
const server = spawn(process.execPath, [path.join(__dirname, "server.js")], {
  env: {
    ...process.env,
    BB_MP_PORT: String(PORT),
    BB_MP_PING_MS: "150",
    BB_MP_CLOCK_FALLBACK_MS: "2000",
  },
  stdio: ["ignore", "pipe", "pipe"],
});

let stdout = "";
server.stdout.on("data", (d) => {
  stdout += d.toString();
});
server.stderr.on("data", (d) => process.stderr.write(d));

function waitForListen() {
  return new Promise((resolve, reject) => {
    const t0 = Date.now();
    const iv = setInterval(() => {
      if (stdout.includes("listening")) {
        clearInterval(iv);
        resolve();
      } else if (Date.now() - t0 > 5000) {
        clearInterval(iv);
        reject(new Error("server did not start: " + stdout));
      }
    }, 50);
  });
}

(async () => {
  try {
    await waitForListen();
    const client = dgram.createSocket("udp4");
    const seen = [];
    let finishedSent = false;
    let clockReplied = false;
    let gotAckBeforeStart = false;
    let positionsSent = false;
    let idxOf12 = -1;

    await new Promise((resolve, reject) => {
      const timer = setTimeout(
        () => reject(new Error("timeout; seen=" + seen.map((t) => "0x" + t.toString(16)).join(","))),
        6000
      );

      client.on("message", (msg) => {
        const p = parsePacket(msg);
        if (!p.ok) return;
        seen.push(p.type);

        if (p.type === 0x0f) {
          assert(p.header.seqMask !== 0 || p.header.peerB === 0, "ack header present");
          if (idxOf12 < 0) gotAckBeforeStart = true;
        }

        // Stub n'envoie plus 0x10 (launcher) — 0x11 après 0x05/0x16.
        if (p.type === 0x16 && !finishedSent) {
          finishedSent = true;
          client.send(
            buildFinishedLoading({ flag: 1, peerA: 1, peerB: 0, seqMask: 0 }),
            PORT,
            "127.0.0.1"
          );
        }

        if (p.type === 0x06 && !clockReplied) {
          clockReplied = true;
          const round = p.payload[1] || 0;
          client.send(
            buildSendClock({ round, clientTime: 12345, headerOpts: { flag: 0 } }),
            PORT,
            "127.0.0.1"
          );
        }

        if (p.type === 0x12) {
          idxOf12 = seen.length - 1;
          if (!positionsSent) {
            positionsSent = true;
            client.send(
              buildPositionsBundle({
                timeMs: 999,
                playerId: 0,
                qx: 100,
                qy: 200,
                qz: 300,
                yawByte: 11,
                pitchByte: 22,
              }),
              PORT,
              "127.0.0.1"
            );
          }
        }

        // Pas d'echo 0x4D : solo = client autoritatif.
        const need = [0x02, 0x05, 0x16, 0x06, 0x08, 0x12, 0x0d, 0x0e];
        if (
          need.every((t) => seen.includes(t)) &&
          gotAckBeforeStart &&
          positionsSent &&
          stdout.includes("PlayerTransform")
        ) {
          clearTimeout(timer);
          resolve();
        }
      });

      client.send(buildDiscover({ magic: GAME_ID }), PORT, "127.0.0.1");
      setTimeout(() => {
        client.send(
          buildClientDetails({
            playerName: "Alice",
            deviceName: "dev1",
            headerOpts: { flag: 1, peerA: 0 },
          }),
          PORT,
          "127.0.0.1"
        );
      }, 60);
    });

    const i06 = seen.indexOf(0x06);
    const i08 = seen.indexOf(0x08);
    const i12 = seen.indexOf(0x12);
    assert(i06 >= 0 && i08 > i06 && i12 > i08, "clock order 0x06→0x08→0x12");
    assert(gotAckBeforeStart, "got 0x0F ACK before StartGame");
    assert(!stdout.includes("PositionsEcho"), "pas d'echo 0x4D PositionsEcho");
    assert(stdout.includes("FinishedLoading"), "log 0x11");
    assert(stdout.includes("SendClock") || stdout.includes("0x07"), "log clock reply");
    assert(stdout.includes("PlayerTransform") || stdout.includes("PositionsEnvelope"), "log positions");

    console.log(
      "UDP keepalive+positions OK (no 0x4D echo):",
      seen.map((t) => "0x" + t.toString(16)).join(", ")
    );
    client.close();
    server.kill();
    process.exit(0);
  } catch (e) {
    console.error(e);
    console.error("--- server stdout ---\n" + stdout);
    server.kill();
    process.exit(1);
  }
})();
