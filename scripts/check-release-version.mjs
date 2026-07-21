import { readFile } from "node:fs/promises";

const root = new URL("../", import.meta.url);
const requestedTag = process.argv[2] ?? process.env.GITHUB_REF_NAME ?? "";
const version = requestedTag.replace(/^v/, "");

if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version)) {
  throw new Error(`Expected a release tag such as v0.2.0, received “${requestedTag || "nothing"}”.`);
}

const [packageSource, tauriSource, cargoSource] = await Promise.all([
  readFile(new URL("package.json", root), "utf8"),
  readFile(new URL("src-tauri/tauri.conf.json", root), "utf8"),
  readFile(new URL("src-tauri/Cargo.toml", root), "utf8"),
]);

const packageVersion = JSON.parse(packageSource).version;
const tauriVersion = JSON.parse(tauriSource).version;
const cargoPackageStart = cargoSource.indexOf("[package]");
const cargoPackageEnd = cargoSource.indexOf("\n[", cargoPackageStart + "[package]".length);
const cargoPackage = cargoPackageStart >= 0
  ? cargoSource.slice(cargoPackageStart, cargoPackageEnd >= 0 ? cargoPackageEnd : undefined)
  : "";
const cargoVersion = cargoPackage.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
const versions = {
  "release tag": version,
  "package.json": packageVersion,
  "src-tauri/tauri.conf.json": tauriVersion,
  "src-tauri/Cargo.toml": cargoVersion,
};

const mismatches = Object.entries(versions).filter(([, candidate]) => candidate !== version);
if (mismatches.length > 0) {
  const details = Object.entries(versions).map(([source, candidate]) => `  ${source}: ${candidate ?? "missing"}`).join("\n");
  throw new Error(`Release versions must match:\n${details}`);
}

console.log(`Release version ${version} is consistent.`);
