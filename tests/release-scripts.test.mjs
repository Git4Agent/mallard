import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";

const projectRoot = resolve(new URL("..", import.meta.url).pathname);

function runScript(script, args) {
  const result = spawnSync(process.execPath, [join(projectRoot, "scripts", script), ...args], {
    cwd: projectRoot,
    encoding: "utf8",
  });
  assert.equal(result.status, 0, result.stderr || result.stdout);
}

test("release scripts publish immutable public URLs and final release notes", async () => {
  const directory = await mkdtemp(join(tmpdir(), "mallard-updater-manifest-"));
  try {
    const assets = {
      "darwin-aarch64": "Mallard_0.2.0_aarch64.app.tar.gz",
      "darwin-aarch64-app": "Mallard_0.2.0_aarch64.app.tar.gz",
      "darwin-x86_64": "Mallard_0.2.0_x64.app.tar.gz",
      "darwin-x86_64-app": "Mallard_0.2.0_x64.app.tar.gz",
      "windows-x86_64": "Mallard_0.2.0_x64-setup.exe",
      "windows-x86_64-nsis": "Mallard_0.2.0_x64-setup.exe",
    };
    const installers = {
      "darwin-aarch64": {
        filename: "Mallard_0.2.0_aarch64.dmg",
        format: "dmg",
        label: "Apple Silicon",
      },
      "darwin-x86_64": {
        filename: "Mallard_0.2.0_x64.dmg",
        format: "dmg",
        label: "Intel",
      },
      "windows-x86_64": {
        filename: "Mallard_0.2.0_x64-setup.exe",
        format: "nsis",
        label: "Windows x64",
      },
    };
    const filenames = new Set([
      ...Object.values(assets),
      ...Object.values(installers).map(({ filename }) => filename),
    ]);
    for (const filename of filenames) {
      await writeFile(join(directory, filename), `artifact:${filename}`);
    }

    const manifestPath = join(directory, "latest.json");
    await writeFile(manifestPath, JSON.stringify({
      version: "0.2.0",
      notes: "Draft notes",
      platforms: Object.fromEntries(Object.entries(assets).map(([platform, filename]) => [
        platform,
        {
          signature: `signature-${platform}`,
          url: `https://github.com/Git4Agent/mallard/releases/download/v0.2.0/${filename}`,
        },
      ])),
    }));

    runScript("prepare-updater-manifest.mjs", [
      manifestPath,
      "https://api.mallard-ai.com/v1/releases",
      "v0.2.0",
    ]);
    let manifest = JSON.parse(await readFile(manifestPath, "utf8"));
    for (const [platform, filename] of Object.entries(assets)) {
      assert.equal(
        manifest.platforms[platform].url,
        `https://api.mallard-ai.com/v1/releases/v0.2.0/${filename}`,
      );
    }
    for (const [platform, expected] of Object.entries(installers)) {
      const bytes = await readFile(join(directory, expected.filename));
      assert.deepEqual(manifest.downloads[platform], {
        url: `https://api.mallard-ai.com/v1/releases/v0.2.0/${expected.filename}`,
        format: expected.format,
        label: expected.label,
        sha256: createHash("sha256").update(bytes).digest("hex"),
        size: bytes.length,
      });
    }

    const notesPath = join(directory, "release-notes.md");
    await writeFile(notesPath, "## Improvements\n\n- Safer updates\n");
    runScript("finalize-updater-manifest.mjs", [
      manifestPath,
      "v0.2.0",
      "https://api.mallard-ai.com/v1/releases",
      notesPath,
    ]);
    manifest = JSON.parse(await readFile(manifestPath, "utf8"));
    assert.equal(manifest.notes, "## Improvements\n\n- Safer updates");
    assert.deepEqual(Object.keys(manifest.downloads).sort(), Object.keys(installers).sort());
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
