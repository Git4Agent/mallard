# PLAN: Portable Claude project paths — `claude --resume` across machines

Status: proposed (2026-07-15).

## Problem

Claude Code stores session transcripts per project under
`~/.claude/projects/<encoded-absolute-cwd>/` (encoding: every
non-alphanumeric character of the cwd becomes `-`, so `/Users/hequ` →
`-Users-hequ`, `/Users/hequ/.ccgui-workspace` → `-Users-hequ--ccgui-workspace`).

Sync copies these dirs byte-faithfully, so a session started under `/A/home`
on machine A lands on machine B as `projects/-A-home/...`. `claude --resume`
run from `/B/home` looks in `projects/-B-home/` — empty. The user's goal:
start a session under `/A/home` on A, push; pull on B and `--resume` it from
`/B/home`, continuing the SAME session (no fork).

The path encoding is machine-specific; everything else about a transcript is
portable. `--resume` finds sessions purely by the encoded directory matching
the current cwd — transcript-internal `cwd` fields are historical metadata
and don't gate resumption. Codex has no such problem (its sessions are
date-nested, path-independent).

## Design: home-normalize the logical path at the Roots waist

`Roots::abs`/`rel` (`lib.rs`) is already the one place logical↔physical
paths diverge (the `agent-sync/**` remap). Add a second remap there, for
`.claude/projects/<first component>` only:

- **Physical → logical (`rel`, push side):** if the encoded project dir name
  starts with `encode(self.home)` at a component boundary (next char is `-`
  or end of name), replace that prefix with the token `~`.
  `projects/-Users-hequ-Desktop-proj/x.jsonl` →
  `.claude/projects/~-Desktop-proj/x.jsonl`; the home itself,
  `projects/-Users-hequ/x.jsonl` → `.claude/projects/~/x.jsonl`.
- **Logical → physical (`abs`, pull side):** a project component starting
  with `~` expands the `~` back to `encode(self.home)` — B materializes
  `projects/-B-home-Desktop-proj/`, exactly where B's `claude --resume`
  looks.

Why this shape:

- The cloud manifest, baselines, and merge machinery all operate on logical
  paths, so both machines sync the SAME logical file — one history, normal
  three-way state matrix, no conflict-copy ping-pong. A post-pull
  rename-the-dirs repair step (the obvious alternative) would break this:
  deletions never propagate, so each machine's rename would re-upload the
  whole dir under its own encoding and resurrect the other's forever.
- `~` cannot occur in a genuinely encoded name (encoder output is
  `[A-Za-z0-9-]`), so the token never collides; `validate_cloud_key` already
  accepts it. Guard the inverse: a physical dir literally named `~...` under
  `projects/` maps to `rel = None` (unsyncable) instead of colliding with
  the token namespace.
- Same-machine round trips are identity (`rel` then `abs` reproduces the
  original path), so existing setups see no churn beyond the one-time
  logical rename below.

Known ceiling (ponytail): the boundary match cannot distinguish
`/Users/hequ/foo` from a sibling literally named `/Users/hequ-foo` — both
encode to `-Users-hequ-foo`. Blind prefix match treats both as home-relative;
failure mode is a foreign machine grouping those sessions under
`<its home>/foo`. Decoding is inherently ambiguous (the readiness scanner
already reads real cwds from transcript first lines instead of decoding —
`project_cwd`, `readiness.rs`); doing per-file transcript reads inside the
hot `rel()` mapping is not worth that corner.

## Scope and non-goals

- **Projects outside home** (`/tmp/x`, other volumes): no token, sync as-is,
  resume works only where the same absolute path exists. Documented, not
  solved.
- **Transcript content is untouched.** Internal `cwd` fields keep the
  origin-machine paths; resume does not care. No content projection, no
  sha churn.
- **`~/.claude.json` project map**: outside the sync roots entirely (see
  AGENT_SYNC_FILE_SETS.md §190) except for relocated-mount setups; foreign
  path keys mean at worst a fresh trust prompt per project on B. Not
  blocking resume; a keyed-union content driver with the same `~` trick is
  a possible follow-up if it ever matters.
- **No migration** (per user, standing rule). Entries pushed before this
  change live in the cloud under raw encodings (`projects/-A-home-...`);
  they remain as inert foreign dirs on other machines (deletions never
  propagate) and the first post-upgrade push re-uploads projects under the
  token paths — one-time re-upload, old cloud entries become dead weight.

## Tests

- Unit: `rel`/`abs` token round trip (home itself, nested project,
  dot-containing project → `~--...`, non-home project untouched, physical
  `~` dir skipped, boundary non-match `-Users-hequx...` untouched).
- Sync scenario (dual-backend): machine A (home a) seeds
  `.claude/projects/<enc(a)>-proj/s.jsonl`, pushes; machine B (home b)
  pulls; assert the file lands at `.claude/projects/<enc(b)>-proj/s.jsonl`
  and the cloud manifest holds `projects/~-proj/s.jsonl`; B edits and
  pushes; A pulls back into its own encoding — converged, no conflict
  copies, no duplicate project dirs.
