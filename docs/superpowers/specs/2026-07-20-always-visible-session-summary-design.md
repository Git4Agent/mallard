# Always-Visible Session Summary Design

## Goal

Make each local Codex session card a permanent two-row layout so session timing and metrics are visible without an information disclosure button.

## Layout and Interaction

The first row continues to show the session title, relative update date, and launch actions. Remove the information button entirely.

The second row is always rendered and contains the existing `ThreadMetrics` summary followed by the existing chat-history control. Start time, end or last-activity time, user turns, token count, agent messages, tool calls, commit appearances, and partial-metrics status retain their current icons, accessible labels, and tooltips.

“Load chat history” remains an independent button. It continues to load and expand conversation turns on demand, changes to “Hide chat history” while open, and controls only the chat-history region beneath the permanent summary row.

## Visual Direction

Retain Mallard's current neutral palette, typography, spacing scale, hover treatment, and compact desktop density. The permanent second row uses the existing summary styles and wraps naturally at narrow widths. No new color, typography, animation, or decorative treatment is introduced; the clearer two-tier information hierarchy is the intentional visual change.

## State and Accessibility

Remove the local session-summary disclosure state, its toggle handler, and its `aria-expanded`/`aria-controls` relationship. Keep the chat-history button's accessible name, expanded state, controlled region, loading state, errors, and pagination behavior unchanged.

Stored-only thread cards and sync-selection behavior remain unchanged unless they already use the shared local `ThreadCard` component.

## Testing

Update the frontend integration test to prove that session metrics and the chat-history button render in the default card markup, and that no “Show session details” information control remains. Retain the focused metric-label and chat-history behavior coverage, then run the frontend test suite and production build.

## Non-goals

- Loading chat history automatically.
- Showing chat turns by default.
- Changing metric values, formatting, or tooltips.
- Redesigning stored-only sessions, commit groups, or sync controls.
