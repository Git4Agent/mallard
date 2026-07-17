import { useEffect, useRef } from "react";
import { LogLine } from "../types";
import Icon from "./Icons";

interface Props {
  lines: LogLine[];
  onClear: () => void;
  onClose: () => void;
}

function formatTs(ts: number): string {
  return new Date(ts).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

export default function LogPanel({ lines, onClear, onClose }: Props) {
  const endRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    endRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [lines]);

  return (
    <div className="log-panel">
      <div className="log-panel-header">
        <span className="log-panel-title">sync log</span>
        <div className="log-panel-actions">
          <button className="btn btn-ghost log-clear-btn" onClick={onClear} aria-label="Clear sync log">
            clear
          </button>
          <button className="btn btn-ghost" onClick={onClose} title="Close" aria-label="Close sync log">
            <Icon name="x" size={13} />
          </button>
        </div>
      </div>
      <div className="log-panel-body">
        {lines.length === 0 ? (
          <div className="log-empty">No entries yet — run a push or pull.</div>
        ) : (
          lines.map((line, i) => (
            <div key={i} className={`log-line log-${line.level}`}>
              <span className="log-ts">{formatTs(line.ts)}</span>
              <span className="log-msg">{line.message}</span>
            </div>
          ))
        )}
        <div ref={endRef} />
      </div>
    </div>
  );
}
