/**
 * Carve DDS blobs from Blitz Brigade sprites.gla → PNG (DXT1/3/5 + uncompressed).
 * Usage: node scripts/extract-map-previews.mjs [path/to/sprites.gla]
 */
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(__dirname, "..");

const DEFAULT_GLA =
  "C:\\Program Files\\WindowsApps\\GAMELOFTSA.BlitzBrigade_1.3.1.13_x86__0pp20fcewvvtj\\sprites.gla";

const glaPath = process.argv[2] || DEFAULT_GLA;
const outDds = path.join(root, "bb-mp-stub", "_re_tmp", "sprites_dds");
const outPng = path.join(root, "bb-mp-stub", "_re_tmp", "sprites_png");

fs.mkdirSync(outDds, { recursive: true });
fs.mkdirSync(outPng, { recursive: true });

function ensureDeps() {
  const need = ["parse-dds", "decode-dxt", "pngjs"];
  for (const name of need) {
    try {
      require.resolve(name);
    } catch {
      console.error(`Missing dep ${name}. Run: npm i -D parse-dds decode-dxt pngjs`);
      process.exit(1);
    }
  }
}

ensureDeps();
const parseDDS = require("parse-dds");
const decodeDXT = require("decode-dxt");
const { PNG } = require("pngjs");

const buf = fs.readFileSync(glaPath);
console.log(`Read ${glaPath} (${buf.length} bytes)`);

const magic = Buffer.from("DDS ");
const offsets = [];
for (let i = 0; i < buf.length - 4; i++) {
  if (buf[i] === 0x44 && buf[i + 1] === 0x44 && buf[i + 2] === 0x53 && buf[i + 3] === 0x20) {
    offsets.push(i);
  }
}
console.log(`Found ${offsets.length} DDS markers`);

function ddsEnd(start, nextStart) {
  // Prefer next marker; else clamp to file end.
  return nextStart ?? buf.length;
}

let carved = 0;
let converted = 0;
const catalog = [];

for (let n = 0; n < offsets.length; n++) {
  const start = offsets[n];
  const end = ddsEnd(start, offsets[n + 1]);
  // Skip tiny / huge junk
  const slice = buf.subarray(start, Math.min(end, start + 8 * 1024 * 1024));
  if (slice.length < 128) continue;

  const id = String(n).padStart(3, "0");
  const ddsPath = path.join(outDds, `sprite_${id}.dds`);
  fs.writeFileSync(ddsPath, slice);
  carved++;

  try {
    const dds = parseDDS(slice.buffer.slice(slice.byteOffset, slice.byteOffset + slice.byteLength));
    const { shape, format, images } = dds;
    const w = shape[0];
    const h = shape[1];
    if (!w || !h || w > 2048 || h > 2048) {
      catalog.push({ id, w, h, format, ok: false, reason: "size" });
      continue;
    }
    const img = images[0];
    // parse-dds renvoie {offset,length} — pas les pixels.
    const data = slice.subarray(img.offset, img.offset + img.length);
    let rgba;
    if (format === "dxt1" || format === "dxt3" || format === "dxt5") {
      const ab = data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength);
      rgba = Buffer.from(decodeDXT(new DataView(ab), w, h, format));
    } else {
      catalog.push({ id, w, h, format, ok: false, reason: "fmt" });
      continue;
    }
    for (let p = 3; p < rgba.length; p += 4) {
      if (rgba[p] < 8 && rgba[p - 3] + rgba[p - 2] + rgba[p - 1] > 8) rgba[p] = 255;
    }

    const png = new PNG({ width: w, height: h });
    rgba.copy(png.data);
    const pngPath = path.join(outPng, `sprite_${id}_${w}x${h}.png`);
    fs.writeFileSync(pngPath, PNG.sync.write(png));
    converted++;
    catalog.push({ id, w, h, format, ok: true, png: path.basename(pngPath) });
  } catch (e) {
    catalog.push({ id, ok: false, reason: String(e.message || e) });
  }
}

fs.writeFileSync(
  path.join(outPng, "catalog.json"),
  JSON.stringify({ glaPath, carved, converted, catalog }, null, 2),
);
console.log(`Carved ${carved} DDS, converted ${converted} PNG → ${outPng}`);
