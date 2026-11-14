import { execSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const conf = JSON.parse(
  fs.readFileSync(path.join(root, "src-tauri", "tauri.conf.json"), "utf8"),
);
const version = conf.version;
const tag = `v${version}`;
const releaseDir = path.join(root, "src-tauri", "target", "release", "bundle");
const latestPath = path.join(root, "latest.json");

function findAsset(dir, pred) {
  if (!fs.existsSync(dir)) return null;
  const name = fs.readdirSync(dir).find(pred);
  return name ? path.join(dir, name) : null;
}

const nsis = findAsset(
  path.join(releaseDir, "nsis"),
  (f) => f.includes(`_${version}_`) && f.endsWith("-setup.exe"),
);
const msi = findAsset(
  path.join(releaseDir, "msi"),
  (f) => f.includes(`_${version}_`) && f.endsWith(".msi"),
);

const assets = [];
for (const file of [nsis, msi, nsis && `${nsis}.sig`, msi && `${msi}.sig`]) {
  if (file && fs.existsSync(file)) assets.push(file);
}
if (!fs.existsSync(latestPath)) {
  console.error("latest.json manquant. Lance make-latest-json avant.");
  process.exit(1);
}
assets.push(latestPath);

if (!nsis && !msi) {
  console.error(`Aucun bundle pour la version ${version}`);
  process.exit(1);
}

function q(arg) {
  return `"${String(arg).replace(/"/g, '\\"')}"`;
}

function gh(args, { inherit = true } = {}) {
  const cmd = ["gh", ...args.map(q)].join(" ");
  execSync(cmd, {
    stdio: inherit ? "inherit" : "pipe",
    cwd: root,
    shell: true,
  });
}

function releaseExists() {
  try {
    gh(["release", "view", tag], { inherit: false });
    return true;
  } catch {
    return false;
  }
}

const notes = process.env.BB_EMU_RELEASE_NOTES || `BB EMU ${version}`;

if (releaseExists()) {
  console.log(`Release ${tag} existe déjà — mise à jour des assets…`);
  gh(["release", "upload", tag, ...assets, "--clobber"]);
} else {
  console.log(`Création release ${tag}…`);
  gh([
    "release",
    "create",
    tag,
    "--title",
    `BB EMU ${version}`,
    "--notes",
    notes,
    ...assets,
  ]);
}

console.log(`OK → https://github.com/SteveRothier/bb-emu/releases/tag/${tag}`);
