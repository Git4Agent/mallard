import { createHash } from "node:crypto";
import { access, readFile, readdir, writeFile } from "node:fs/promises";
import { basename, dirname, join } from "node:path";

const [manifestPath, publicBaseUrl, releaseTag] = process.argv.slice(2);
if (!manifestPath || !publicBaseUrl || !releaseTag) {
  throw new Error("Usage: prepare-updater-manifest.mjs <latest.json> <public-base-url> <release-tag>");
}
if (!/^v?\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(releaseTag)) {
  throw new Error(`Invalid release tag “${releaseTag}”.`);
}

const manifest = JSON.parse(await readFile(manifestPath, "utf8"));
const expectedVersion = releaseTag.replace(/^v/, "");
if (String(manifest.version).replace(/^v/, "") !== expectedVersion) {
  throw new Error(`Updater manifest version ${manifest.version} does not match ${releaseTag}.`);
}

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

const releaseDirectory = dirname(manifestPath);
const publicReleaseUrl = `${publicBaseUrl.replace(/\/$/, "")}/${encodeURIComponent(releaseTag)}`;
for (const [platform, entry] of Object.entries(manifest.platforms)) {
  if (!/^[a-z0-9][a-z0-9_-]{0,79}$/.test(platform)) {
    throw new Error(`Updater manifest contains an unsafe platform key “${platform}”.`);
  }
  if (!entry || typeof entry !== "object" || typeof entry.url !== "string"
    || typeof entry.signature !== "string" || !entry.signature.trim()) {
    throw new Error(`Updater manifest is missing a complete ${platform} entry.`);
  }

  const sourceUrl = new URL(entry.url);
  const filename = decodeURIComponent(basename(sourceUrl.pathname));
  if (!/^[A-Za-z0-9][A-Za-z0-9._+()-]{0,199}$/.test(filename)) {
    throw new Error(`Unsafe updater asset name “${filename}”.`);
  }
  await access(join(releaseDirectory, filename));
  entry.url = `${publicReleaseUrl}/${encodeURIComponent(filename)}`;
}

const releaseFiles = await readdir(releaseDirectory);
const downloadSpecifications = [
  {
    platform: "darwin-aarch64",
    pattern: /_(?:aarch64|arm64)\.dmg$/i,
    format: "dmg",
    label: "Apple Silicon",
  },
  {
    platform: "darwin-x86_64",
    pattern: /_(?:x64|x86_64)\.dmg$/i,
    format: "dmg",
    label: "Intel",
  },
  {
    platform: "windows-x86_64",
    pattern: /_(?:x64|x86_64)-setup\.exe$/i,
    format: "nsis",
    label: "Windows x64",
  },
];

manifest.downloads = {};
for (const specification of downloadSpecifications) {
  const matches = releaseFiles.filter((filename) => specification.pattern.test(filename));
  if (matches.length !== 1) {
    throw new Error(
      `Expected exactly one ${specification.platform} installer, found ${matches.length}.`,
    );
  }
  const filename = matches[0];
  if (!/^[A-Za-z0-9][A-Za-z0-9._+()-]{0,199}$/.test(filename)) {
    throw new Error(`Unsafe installer asset name “${filename}”.`);
  }
  const bytes = await readFile(join(releaseDirectory, filename));
  manifest.downloads[specification.platform] = {
    url: `${publicReleaseUrl}/${encodeURIComponent(filename)}`,
    format: specification.format,
    label: specification.label,
    sha256: createHash("sha256").update(bytes).digest("hex"),
    size: bytes.length,
  };
}

await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`, "utf8");
console.log(`Prepared public updater manifest for ${releaseTag}.`);
