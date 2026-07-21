import { readFile, writeFile } from "node:fs/promises";

const [manifestPath, releaseTag, publicBaseUrl, releaseNotesPath] = process.argv.slice(2);
if (!manifestPath || !releaseTag || !publicBaseUrl || !releaseNotesPath) {
  throw new Error(
    "Usage: finalize-updater-manifest.mjs <latest.json> <release-tag> <public-base-url> <release-notes.md>",
  );
}

const manifest = JSON.parse(await readFile(manifestPath, "utf8"));
const expectedVersion = releaseTag.replace(/^v/, "");
if (String(manifest.version).replace(/^v/, "") !== expectedVersion) {
  throw new Error(`Updater manifest version ${manifest.version} does not match ${releaseTag}.`);
}

const expectedPrefix = `${publicBaseUrl.replace(/\/$/, "")}/${encodeURIComponent(releaseTag)}/`;
const requiredPlatforms = [
  "darwin-aarch64",
  "darwin-aarch64-app",
  "darwin-x86_64",
  "darwin-x86_64-app",
  "windows-x86_64",
  "windows-x86_64-nsis",
];
if (!manifest.platforms || typeof manifest.platforms !== "object" || Array.isArray(manifest.platforms)) {
  throw new Error("Updater manifest is missing its platform map.");
}
for (const platform of requiredPlatforms) {
  if (!manifest.platforms[platform]) {
    throw new Error(`Updater manifest is missing a complete ${platform} entry.`);
  }
}

for (const [platform, entry] of Object.entries(manifest.platforms)) {
  if (!/^[a-z0-9][a-z0-9_-]{0,79}$/.test(platform)) {
    throw new Error(`Updater manifest contains an unsafe platform key “${platform}”.`);
  }
  if (!entry || typeof entry !== "object" || typeof entry.url !== "string"
    || typeof entry.signature !== "string" || !entry.signature.trim()) {
    throw new Error(`Updater manifest is missing a complete ${platform} entry.`);
  }
  if (!entry.url.startsWith(expectedPrefix)) {
    throw new Error(`${platform} does not use the immutable public release URL.`);
  }
}

const requiredDownloads = new Map([
  ["darwin-aarch64", "dmg"],
  ["darwin-x86_64", "dmg"],
  ["windows-x86_64", "nsis"],
]);
if (!manifest.downloads || typeof manifest.downloads !== "object" || Array.isArray(manifest.downloads)) {
  throw new Error("Updater manifest is missing its website download map.");
}
for (const [platform, expectedFormat] of requiredDownloads) {
  const entry = manifest.downloads[platform];
  if (!entry || typeof entry !== "object") {
    throw new Error(`Updater manifest is missing a complete ${platform} website download.`);
  }
  if (typeof entry.url !== "string" || !entry.url.startsWith(expectedPrefix)) {
    throw new Error(`${platform} website download does not use the immutable public release URL.`);
  }
  if (entry.format !== expectedFormat || typeof entry.label !== "string" || !entry.label.trim()) {
    throw new Error(`Updater manifest has invalid ${platform} website download metadata.`);
  }
  if (!/^[a-f0-9]{64}$/.test(entry.sha256) || !Number.isSafeInteger(entry.size) || entry.size <= 0) {
    throw new Error(`Updater manifest has invalid ${platform} website download integrity metadata.`);
  }
}

const releaseNotes = (await readFile(releaseNotesPath, "utf8")).trim();
manifest.notes = releaseNotes || "This release includes improvements and fixes.";
await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`, "utf8");
console.log(`Finalized public updater manifest for ${releaseTag}.`);
