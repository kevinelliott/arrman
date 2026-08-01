import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const composeSource = readFileSync(join(root, "src/compose.rs"), "utf8");
const catalog = readFileSync(join(root, "docs/SUPPORTED_SERVICES.md"), "utf8");
const readme = readFileSync(join(root, "README.md"), "utf8");

const purposeBlock = composeSource.match(
  /const PURPOSES: &\[\(&str, &str\)\] = &\[(.*?)\n\];/s,
);
if (!purposeBlock) {
  throw new Error("could not locate the PURPOSES service catalog");
}

const sourceNames = [...purposeBlock[1].matchAll(/\(\s*"([a-z0-9_-]+)"\s*,\s*"[^"]+"\s*,?\s*\)/gs)]
  .map((match) => match[1])
  .sort();
const catalogTable = catalog.split("## Owned dependency groups", 1)[0];
const documentedNames = [...catalogTable.matchAll(/^\| `([a-z0-9_-]+)` \|/gm)]
  .map((match) => match[1])
  .sort();

if (sourceNames.length === 0) {
  throw new Error("the source service catalog unexpectedly parsed as empty");
}
if (JSON.stringify(sourceNames) !== JSON.stringify(documentedNames)) {
  const missing = sourceNames.filter((name) => !documentedNames.includes(name));
  const stale = documentedNames.filter((name) => !sourceNames.includes(name));
  throw new Error(`supported-service docs are stale; missing=${missing.join(",") || "none"}; stale=${stale.join(",") || "none"}`);
}

for (const asset of ["docs/assets/tui.png", "docs/assets/web-control-room.png"]) {
  if (!existsSync(join(root, asset))) {
    throw new Error(`README asset is missing: ${asset}`);
  }
  if (!readme.includes(asset)) {
    throw new Error(`README does not embed ${asset}`);
  }
}

for (const required of [
  "https://github.com/kevinelliott/arrman/actions/workflows/ci.yml",
  "https://github.com/kevinelliott/arrman/releases/latest",
  "docs/SUPPORTED_SERVICES.md",
  "CONTRIBUTING.md",
]) {
  if (!readme.includes(required)) {
    throw new Error(`README is missing required project link: ${required}`);
  }
}

if (readme.includes("192.168.13.158") || catalog.includes("192.168.13.158")) {
  throw new Error("public documentation contains a private installation address");
}

console.log(`documentation catalog verified: ${sourceNames.length} services`);
