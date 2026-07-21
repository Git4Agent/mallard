import { useEffect, useState } from "react";
import AppUpdater from "./components/AppUpdater";
import ProjectSyncV3 from "./components/project-sync/ProjectSyncV3";
import type { AppTheme } from "./types";
import { applyTheme, getStoredTheme } from "./theme";
import "./App.css";

export default function App() {
  const [theme, setTheme] = useState<AppTheme>(getStoredTheme);
  const [busy, setBusy] = useState(false);

  useEffect(() => applyTheme(theme), [theme]);

  return (
    <>
      <ProjectSyncV3
        theme={theme}
        onThemeChange={setTheme}
        onBusyChange={setBusy}
      />
      <AppUpdater busy={busy} />
    </>
  );
}
