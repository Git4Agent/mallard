# Repository Guidelines

## Project Structure & Module Organization

This is a Tauri 2 desktop app with a React/Vite frontend.

- `src/` contains the TypeScript React app.
- `src/components/` contains UI components such as `FileTree`, `FilePreview`, and `SyncPanel`.
- `src/types.ts` holds shared frontend types that mirror Rust response shapes.
- `src-tauri/src/` contains Rust commands exposed through Tauri `invoke`.
- `src-tauri/capabilities/` and `src-tauri/tauri.conf.json` hold Tauri permissions and app configuration.
- `public/` and `src/assets/` contain static frontend assets.

## Build, Test, and Development Commands

- `npm install` installs JavaScript and Tauri CLI dependencies.
- `npm run dev` starts the Vite frontend only.
- `npm run tauri dev` runs the desktop app with the Rust backend and Vite frontend.
- `npm run build` runs TypeScript checks and builds the frontend bundle.
- `npm run tauri build` creates a packaged desktop build.
- `cd src-tauri && cargo check` type-checks the Rust backend without packaging.

## Coding Style & Naming Conventions

Use strict TypeScript and keep React components in PascalCase files, for example `SyncPanel.tsx`. Use camelCase for TypeScript variables and functions. Rust command payloads currently use snake_case fields, matching serde output consumed by the frontend.

Prefer small functions near their callers. Reuse the existing Tauri `invoke` command pattern instead of adding a state layer for simple UI/backend calls. Keep comments for non-obvious behavior, especially file filtering or sync safety decisions.

## Testing Guidelines

There is no committed test framework yet. For now, run `npm run build` and `cd src-tauri && cargo check` before handing off changes. When adding meaningful logic, add the smallest useful check: Rust unit tests near backend helpers, or a lightweight frontend test only if a test runner is introduced in the same change.

Name future frontend tests `*.test.ts` or `*.test.tsx`; name Rust tests after the behavior being protected.

## Commit & Pull Request Guidelines

This checkout does not include Git history, so use concise imperative commits such as `Add sync config validation` or `Fix file tree selection`.

Pull requests should include a short problem statement, the user-visible behavior changed, verification commands run, and screenshots or screen recordings for UI changes.

## Security & Configuration Tips

This app reads and writes `~/.codex` files and stores a sync endpoint plus bearer token in app data. Do not commit local secrets, tokens, generated bundles, or personal Codex data. Test destructive restore paths against disposable files or a backed-up `~/.codex` copy.
