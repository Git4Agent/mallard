# Mallard Project History Design System

## Direction

Preserve the landed native desktop utility aesthetic. This is a compact developer-tool history view, not a dashboard or marketing page. The one signature element is a quiet first-parent commit rail: a one-pixel vertical line with compact commit nodes that visibly anchors repeated thread references to Git history.

## Tokens

Use only the existing CSS variables in `src/App.css`. Dark and light themes are equally supported. Primary surfaces are `--bg-0`, `--bg-1`, `--surface-card`, and `--surface-row`; text uses `--text-0` through `--text-3`; semantic states use `--accent`, `--green`, `--yellow`, and `--red`. Use `--font-mono` for SHA/branch data and `--font` for prose. Do not add web fonts.

## Density and geometry

- Existing workspace content width: `max-width: 1180px`.
- Page inset: `52px clamp(24px, 3.2vw, 48px) 64px`.
- Compact rows/cards: 8–14px internal spacing, 8px corner radius, one-pixel token borders.
- Sidebar remains resizable at its existing limits; the compact repository-type chip plus settings/remove actions must fit at the minimum width.
- Branch selector remains visible at the page header while scrolling.

## Components and behavior

- Header: local alias/repository name plus “activity”; canonical directory, Codex configuration, storage Pull/Push activity, and shared repository name when aliased.
- Toolbar: semantic native select and Refresh button; loading icon spins without shifting layout.
- Commit rail: newest first, first-parent only, SHA/time/subject in one compact row, thread cards below.
- Thread cards: title, explicit start/end or active date, neutral metrics, repeated-commit text, lazy details, and two explicit launch buttons. Do not show prompt extracts or mapping badges.
- Non-Git: same header and a flat “Codex threads” list; do not render fake Git controls.
- Uncommitted Changes: sessions with neither an overlapping nor 24-hour follow-up commit; this is not current `git status`.
- Missing profile/error/empty states always include a recovery action and use `role="alert"` for failures.
- All icon-only controls require `title` and `aria-label`; focus remains visible; no decorative motion beyond existing 120–200ms transitions.

## Deliberate exclusions

No gradients, glass effects, oversized typography, charts, avatars, colored commit ownership claims, or full-DAG visualization. Mapping is temporal context, not causal attribution.
