import { useEffect, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { isTauri } from "@tauri-apps/api/core";
import Icon from "./Icons";

export const APP_UPDATE_CHECK_EVENT = "mallard:check-for-updates";

export function requestAppUpdateCheck(target: EventTarget = window): void {
  target.dispatchEvent(new Event(APP_UPDATE_CHECK_EVENT));
}

export default function AppUpdateControl() {
  const [supported, setSupported] = useState(false);
  const [version, setVersion] = useState<string | null>(null);

  useEffect(() => {
    if (!isTauri()) return;
    let active = true;
    setSupported(true);
    void getVersion()
      .then((value) => {
        if (active) setVersion(value);
      })
      .catch(() => {
        if (active) setVersion(null);
      });
    return () => {
      active = false;
    };
  }, []);

  return (
    <footer className="v3-sidebar-footer app-update-control">
      <button
        type="button"
        onClick={() => requestAppUpdateCheck()}
        disabled={!supported}
        title={supported ? "Check for Mallard updates" : "Update checks are available in the desktop app"}
        aria-label="Check for Mallard updates"
      >
        <Icon name="refresh" size={14} />
        <span>Check for updates</span>
        {version && <small>v{version}</small>}
      </button>
    </footer>
  );
}
