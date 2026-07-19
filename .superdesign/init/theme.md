# Theme

Source: selected complete token blocks and history-shell selectors from `src/App.css`. The stylesheet is 6,000+ lines, so only the page-relevant sections are included per Superdesign's large-file rule.

```css
:root {
  --color-scheme: dark;
  --bg-0: #151515; --bg-1: #242424; --bg-2: #2f2f2f; --bg-3: #383838; --bg-4: #1c1c1c;
  --bg-hover: rgba(255,255,255,.055); --bg-active: rgba(255,255,255,.095);
  --border: rgba(255,255,255,.075); --border-mid: rgba(255,255,255,.16); --border-strong: rgba(255,255,255,.24);
  --button-bg: #202020; --button-bg-hover: #292929; --button-border: rgba(255,255,255,.14);
  --surface-card: #181818; --surface-row: #191919; --surface-row-hover: #1c1c1c;
  --surface-subtle: rgba(255,255,255,.025); --chip-bg: rgba(255,255,255,.05);
  --danger-soft: rgba(248,113,113,.09); --warning-soft: rgba(251,191,36,.09); --accent-soft: rgba(134,189,251,.08);
  --text-0: #ededed; --text-1: #c6c6c6; --text-2: #898989; --text-3: #616161;
  --accent: #86bdfb; --green: #4ade80; --red: #f87171; --yellow: #fbbf24; --purple: #a78bfa;
  --font: -apple-system, BlinkMacSystemFont, 'Inter', 'Segoe UI', 'Helvetica Neue', sans-serif;
  --font-mono: 'SF Mono', 'JetBrains Mono', 'Menlo', 'Cascadia Code', 'Consolas', monospace;
  --r-lg: 22px; --r: 14px; --r-sm: 8px; --r-pill: 999px;
  --shadow-card: 0 18px 54px rgba(0,0,0,.28);
}
:root[data-theme="light"] {
  --color-scheme: light;
  --bg-0: #f6f7f8; --bg-1: #eef0f2; --bg-2: #e4e7ea; --bg-3: #d9dde1; --bg-4: #fff;
  --bg-hover: rgba(24,32,44,.05); --bg-active: rgba(24,32,44,.09);
  --border: rgba(24,32,44,.09); --border-mid: rgba(24,32,44,.16); --border-strong: rgba(24,32,44,.25);
  --button-bg: #fff; --button-bg-hover: #f0f2f4; --button-border: rgba(24,32,44,.16);
  --surface-card: #fff; --surface-row: #fbfcfd; --surface-row-hover: #f2f4f6;
  --text-0: #20242a; --text-1: #434a54; --text-2: #6d7580; --text-3: #9198a1;
  --accent: #397dcc; --green: #238636; --red: #d1242f; --yellow: #9a6700; --purple: #7c3aed;
}
body { font-family: var(--font); font-size: 14px; background: var(--bg-0); color: var(--text-0); line-height: 1.5; }
.v3-sidebar { width: var(--v3-sidebar-width, 318px); min-width: var(--v3-sidebar-width, 318px); background: var(--bg-1); }
.v3-project-links-page { padding: 52px clamp(24px, 3.2vw, 48px) 64px; }
.v3-project-links-page .profile-links-section { width: 100%; max-width: 1180px; margin: 0 auto; }
.sidebar-profile-item { min-height: 40px; display:flex; align-items:center; gap:3px; padding:3px 5px 3px 8px; border-radius:var(--r-sm); }
.sidebar-profile-actions button { width:27px; height:27px; border:0; border-radius:6px; background:transparent; color:var(--text-2); }
```
