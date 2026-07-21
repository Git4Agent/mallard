import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { join, resolve } from "node:path";

const args = process.argv.slice(2);

function run(command, commandArgs) {
  return new Promise((resolveRun, rejectRun) => {
    const child = spawn(command, commandArgs, { stdio: "inherit" });
    child.once("error", rejectRun);
    child.once("exit", (code) => resolveRun(code ?? 1));
  });
}

function macosAppPath(buildArgs) {
  const targetIndex = buildArgs.indexOf("--target");
  const target = targetIndex >= 0 ? buildArgs[targetIndex + 1] : undefined;
  if (targetIndex >= 0 && !target) throw new Error("--target requires a Rust target triple.");

  return resolve(
    "src-tauri",
    "target",
    ...(target ? [target] : []),
    "release",
    "bundle",
    "macos",
    "Mallard.app",
  );
}

const exitCode = await run(process.platform === "win32" ? "tauri.cmd" : "tauri", args);
if (exitCode !== 0) process.exit(exitCode);

if (process.env.MALLARD_VERIFY_MACOS_BUNDLE === "1") {
  if (process.platform !== "darwin") {
    throw new Error("MALLARD_VERIFY_MACOS_BUNDLE is only supported on macOS release runners.");
  }
  if (args[0] !== "build") {
    throw new Error("MALLARD_VERIFY_MACOS_BUNDLE requires the Tauri build command.");
  }

  const appPath = macosAppPath(args);
  if (!existsSync(appPath)) throw new Error(`Expected macOS app bundle at ${appPath}.`);

  const verificationCode = await run("codesign", ["--verify", "--deep", "--strict", "--verbose=4", appPath]);
  if (verificationCode !== 0) process.exit(verificationCode);
}
