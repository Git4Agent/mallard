import { spawnSync } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const outputDirectory = await mkdtemp(join(tmpdir(), "agent-sync-frontend-tests-"));
const outputFile = join(outputDirectory, "pull-review.integration.test.cjs");

try {
  await build({
    absWorkingDir: projectRoot,
    entryPoints: ["tests/frontend/pull-review.integration.test.tsx"],
    outfile: outputFile,
    bundle: true,
    platform: "node",
    format: "cjs",
    target: "node20",
    define: { "process.env.NODE_ENV": '"test"' },
    logLevel: "warning",
  });
  const result = spawnSync(process.execPath, ["--test", outputFile], {
    cwd: projectRoot,
    stdio: "inherit",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exitCode = result.status ?? 1;
} finally {
  await rm(outputDirectory, { recursive: true, force: true });
}
