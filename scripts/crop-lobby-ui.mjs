/**
 * Crop lobby map thumbs (sprite_060) + UI chrome from sprites_png → public/
 */
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const { PNG } = require("pngjs");

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const pngDir = path.join(root, "bb-mp-stub", "_re_tmp", "sprites_png");
const mapsOut = path.join(root, "public", "maps");
const uiOut = path.join(root, "public", "ui");

fs.mkdirSync(mapsOut, { recursive: true });
fs.mkdirSync(uiOut, { recursive: true });

function loadPng(name) {
  return PNG.sync.read(fs.readFileSync(path.join(pngDir, name)));
}

function crop(src, x, y, w, h) {
  const dst = new PNG({ width: w, height: h });
  for (let row = 0; row < h; row++) {
    for (let col = 0; col < w; col++) {
      const sx = Math.min(src.width - 1, Math.max(0, x + col));
      const sy = Math.min(src.height - 1, Math.max(0, y + row));
      const si = (sy * src.width + sx) * 4;
      const di = (row * w + col) * 4;
      dst.data[di] = src.data[si];
      dst.data[di + 1] = src.data[si + 1];
      dst.data[di + 2] = src.data[si + 2];
      dst.data[di + 3] = src.data[si + 3];
    }
  }
  return dst;
}

function save(outPath, png) {
  fs.writeFileSync(outPath, PNG.sync.write(png));
  console.log("wrote", path.relative(root, outPath), `${png.width}x${png.height}`);
}

// --- Map thumbs from sprite_060 top row (6 vertical cells) ---
const sheet = loadPng("sprite_060_1024x1024.png");
// Empirically: top row ≈ y 8..340, 6 equal columns with small gaps
const cols = 6;
const cellW = Math.floor(sheet.width / cols);
const topY = 4;
const topH = 336;
const strips = [];
for (let i = 0; i < cols; i++) {
  const x = i * cellW + 4;
  const w = cellW - 8;
  strips.push(crop(sheet, x, topY, w, topH));
  save(path.join(mapsOut, `_strip_${i}.png`), strips[i]);
}

// Wire order after visual triage (adjust if needed):
// 0 Cat, 1 Mada, 2 Ruines, 3 Quais, 4 Fort, 5 OVNI
// strip visual: 0 plaza/palm, 1 pier, 2 fire/industrial, 3 windmill, 4 beach, 5 jungle
const wireFromStrip = [3, 4, 5, 0, 1, 2]; // will refine after reading crops
// Actually write both strip dumps; final mapping set below after inspection helper.

// Beauty shots for lobby preview panel (right). Madagascar = lobby screenshot crop.
const beauty = {
  0: { file: "sprite_094_1024x1024.png", hFrac: 0.72 }, // Cathédrale (loading shot)
  2: { file: "sprite_106_1024x1024.png", hFrac: 0.72 }, // Ruines
  3: { file: "sprite_087_1024x1024.png", hFrac: 0.55 }, // Quais (Malte)
  4: { file: "sprite_095_1024x1024.png", hFrac: 0.72 }, // Fort
};

function trimGrayPad(src, y0, hMax) {
  let last = y0 + hMax - 1;
  for (let row = last; row > y0; row -= 2) {
    let gray = 0,
      n = 0;
    for (let x = 0; x < src.width; x += 8) {
      const i = (row * src.width + x) * 4;
      const r = src.data[i],
        g = src.data[i + 1],
        b = src.data[i + 2],
        a = src.data[i + 3];
      if (a < 200) continue;
      n++;
      if (Math.abs(r - g) < 12 && Math.abs(g - b) < 12 && r > 95 && r < 155) gray++;
    }
    if (n && gray / n < 0.85) {
      last = row;
      break;
    }
  }
  return Math.max(120, last - y0 + 1);
}

for (const [id, spec] of Object.entries(beauty)) {
  const n = Number(id);
  if (spec.file && fs.existsSync(path.join(pngDir, spec.file))) {
    const src = loadPng(spec.file);
    const hMax = Math.floor(src.height * spec.hFrac);
    const h = trimGrayPad(src, 0, hMax);
    save(path.join(mapsOut, `map-${n}.png`), crop(src, 0, 0, src.width, h));
  }
}

// Madagascar: authentic lobby preview (crashed plane) from lobby screenshot crop
const madaLobby = path.join(root, "bb-mp-stub", "_re_tmp", "mada_from_shot_520.png");
if (fs.existsSync(madaLobby)) {
  save(path.join(mapsOut, "map-1.png"), PNG.sync.read(fs.readFileSync(madaLobby)));
} else {
  console.warn("missing madagascar lobby crop:", madaLobby);
}

// OVNI: sprite_060 strip index 2 (snow / crash-site action thumb)
save(path.join(mapsOut, "map-5.png"), strips[2]);
console.log("strips dumped; lobby map previews written");

// --- Chrome from sprite_024 / sprite_026 ---
const ui = loadPng("sprite_024_2048x1024.png");
// Cream-ish horizontal bar region (approx from atlas layout) — scan for cream pixels
function findCreamBar(img) {
  // Sample rows for high cream density
  let best = { y: 700, score: 0, x0: 100, x1: 500 };
  for (let y = 400; y < img.height - 40; y += 4) {
    let run = 0,
      bestRun = 0,
      runStart = 0,
      bestStart = 0;
    for (let x = 0; x < img.width; x++) {
      const i = (y * img.width + x) * 4;
      const r = img.data[i],
        g = img.data[i + 1],
        b = img.data[i + 2],
        a = img.data[i + 3];
      const cream = a > 200 && r > 180 && g > 170 && b > 140 && r - b < 80;
      if (cream) {
        if (run === 0) runStart = x;
        run++;
        if (run > bestRun) {
          bestRun = run;
          bestStart = runStart;
        }
      } else run = 0;
    }
    if (bestRun > best.score) best = { y, score: bestRun, x0: bestStart, x1: bestStart + bestRun };
  }
  return best;
}

const bar = findCreamBar(ui);
console.log("cream bar", bar);
if (bar.score > 80) {
  const pad = 4;
  const bh = 48;
  save(
    path.join(uiOut, "panel-cream.png"),
    crop(ui, bar.x0 - pad, bar.y - 8, Math.min(imgW(ui, bar), 420), bh),
  );
}
function imgW(img, bar) {
  return Math.min(img.width - (bar.x0 - 4), Math.max(120, bar.x1 - bar.x0 + 8));
}

// Arrows: try sprite_026 square cream arrows
const ui2Path = path.join(pngDir, "sprite_026_1024x1024.png");
if (fs.existsSync(ui2Path)) {
  const ui2 = loadPng("sprite_026_1024x1024.png");
  // Heuristic: search bright olive/cream squares with asymmetry — dump several candidates
  // Fallback: crop known-ish regions (left side often has arrow buttons)
  save(path.join(uiOut, "arrow-sheet.png"), crop(ui2, 0, 0, 512, 512));
}

console.log("done crop-lobby-ui (pass 1)");
