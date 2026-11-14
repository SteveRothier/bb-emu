import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const conf = JSON.parse(
  fs.readFileSync(path.join(root, "src-tauri", "tauri.conf.json"), "utf8"),
);
const version = conf.version;
const releaseDir = path.join(root, "src-tauri", "target", "release", "bundle");

function findAsset(dir, pred) {
  if (!fs.existsSync(dir)) return null;
  const files = fs.readdirSync(dir);
  const name = files.find(pred);
  return name ? path.join(dir, name) : null;
}

const nsis = findAsset(path.join(releaseDir, "nsis"), (f) => f.endsWith("-setup.exe"));
const msi = findAsset(path.join(releaseDir, "msi"), (f) => f.endsWith(".msi"));
const asset = nsis || msi;

if (!asset) {
  console.error("Aucun bundle NSIS/MSI trouvé. Lance d'abord build.bat");
  process.exit(1);
}

const sigPath = `${asset}.sig`;
if (!fs.existsSync(sigPath)) {
  console.error(`Signature manquante: ${sigPath}`);
  console.error("Vérifie TAURI_SIGNING_PRIVATE_KEY lors du build.");
  process.exit(1);
}

const signature = fs.readFileSync(sigPath, "utf8").trim();
const fileName = path.basename(asset);

// Remplace cette URL par l'URL réelle de ton release GitHub (ou autre host)
const baseUrl =
  process.env.BB_EMU_RELEASE_BASE ||
  `https://github.com/SteveRothier/bb-emu/releases/download/v${version}`;

const latest = {
  version,
  notes: process.env.BB_EMU_RELEASE_NOTES || `BB EMU ${version}`,
  pub_date: new Date().toISOString(),
  platforms: {
    "windows-x86_64": {
      signature,
      url: `${baseUrl}/${fileName}`,
    },
  },
};

const out = path.join(root, "latest.json");
fs.writeFileSync(out, JSON.stringify(latest, null, 2) + "\n");
console.log(`Écrit ${out}`);
console.log(`Asset: ${fileName}`);
console.log(`URL:   ${latest.platforms["windows-x86_64"].url}`);
console.log("Uploade latest.json + le bundle + le .sig sur ta release.");
