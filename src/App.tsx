import { useEffect, useState } from "react";
import ProjectSyncV3 from "./components/project-sync/ProjectSyncV3";
import type { AppTheme } from "./types";
import { applyTheme, getStoredTheme } from "./theme";
import "./App.css";

export default function App() {
  const [theme, setTheme] = useState<AppTheme>(getStoredTheme);

  useEffect(() => applyTheme(theme), [theme]);

  return <ProjectSyncV3 theme={theme} onThemeChange={setTheme} />;
}
