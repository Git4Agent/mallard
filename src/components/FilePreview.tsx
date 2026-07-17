import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { confirm } from "@tauri-apps/plugin-dialog";
import CodeMirror from "@uiw/react-codemirror";
import { EditorView, keymap } from "@codemirror/view";
import { json } from "@codemirror/lang-json";
import { markdown } from "@codemirror/lang-markdown";
import { yaml } from "@codemirror/lang-yaml";
import { StreamLanguage } from "@codemirror/language";
import { toml } from "@codemirror/legacy-modes/mode/toml";
import { shell } from "@codemirror/legacy-modes/mode/shell";
import { oneDark } from "@codemirror/theme-one-dark";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { AppTheme, FileDocument } from "../types";
import Icon from "./Icons";

interface Props {
  path: string;
  theme: AppTheme;
  /** A save landed on disk — statuses should refresh. */
  onSaved?: () => void;
  /** Unsaved edits exist; the app guards navigation away from the editor. */
  onDirtyChange?: (dirty: boolean) => void;
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes}B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)}K`;
  return `${(bytes / 1024 / 1024).toFixed(1)}M`;
}

function detectLang(filename: string): string {
  const ext = filename.split(".").pop()?.toLowerCase() ?? "";
  const map: Record<string, string> = {
    json: "json", jsonl: "jsonl", toml: "toml", md: "markdown",
    yml: "yaml", yaml: "yaml", sh: "shell", ts: "ts", js: "js",
    txt: "text", env: "env",
  };
  return map[ext] ?? "text";
}

function isMarkdownFile(filename: string): boolean {
  const ext = filename.split(".").pop()?.toLowerCase() ?? "";
  return ext === "md" || ext === "markdown";
}

function displayPathParts(path: string): string[] {
  const parts = path.split(/[\\/]/).filter(Boolean);
  const usersIndex = parts.findIndex((part, index) => part === "Users" && index <= 1);
  if (usersIndex >= 0 && parts.length > usersIndex + 2) {
    return parts.slice(usersIndex + 2);
  }
  return parts;
}

function languageFor(filename: string) {
  const ext = filename.split(".").pop()?.toLowerCase() ?? "";
  switch (ext) {
    case "json":
    case "jsonl":
      return [json()];
    case "md":
    case "markdown":
      return [markdown()];
    case "yml":
    case "yaml":
      return [yaml()];
    case "toml":
      return [StreamLanguage.define(toml)];
    case "sh":
    case "zsh":
    case "bash":
      return [StreamLanguage.define(shell)];
    default:
      return [];
  }
}

// Keeps CodeMirror on the same surface as the rest of the selected app theme.
function appEditorTheme(theme: AppTheme) {
  return EditorView.theme({
    "&": { backgroundColor: "transparent", fontSize: "12.5px" },
    ".cm-content": { fontFamily: "var(--font-mono)", padding: "14px 0" },
    ".cm-gutters": {
      backgroundColor: "transparent",
      color: "var(--text-3)",
      border: "none",
      paddingLeft: "10px",
    },
    ".cm-activeLine": { backgroundColor: "var(--editor-active-line)" },
    ".cm-activeLineGutter": { backgroundColor: "transparent", color: "var(--text-1)" },
    "&.cm-focused": { outline: "none" },
  }, { dark: theme === "dark" });
}

export default function FilePreview({ path, theme, onSaved, onDirtyChange }: Props) {
  const [doc, setDoc] = useState<FileDocument | null>(null);
  const [text, setText] = useState("");
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [savedFlash, setSavedFlash] = useState(false);

  const shaRef = useRef("");
  const textRef = useRef(text);
  textRef.current = text;
  const docRef = useRef(doc);
  docRef.current = doc;
  const savingRef = useRef(false);

  const pathParts = path.split(/[\\/]/).filter(Boolean);
  const filename = pathParts[pathParts.length - 1] ?? path;
  const breadcrumbParts = displayPathParts(path);
  const breadcrumbAncestors = breadcrumbParts.slice(0, -1);

  // Markdown opens rendered; a toggle switches to the editor. Everything
  // else goes straight to the editor.
  const isMarkdown = isMarkdownFile(filename);
  const [mode, setMode] = useState<"preview" | "edit">(isMarkdown ? "preview" : "edit");
  useEffect(() => {
    setMode(isMarkdownFile(path.split("/").pop() ?? "") ? "preview" : "edit");
  }, [path]);

  const load = useCallback(async () => {
    setLoadError(null);
    setSaveError(null);
    setDoc(null);
    setText("");
    try {
      const d = await invoke<FileDocument>("read_file_content", { path });
      setDoc(d);
      setText(d.content);
      shaRef.current = d.sha256;
    } catch (e) {
      setLoadError(String(e));
    }
  }, [path]);

  useEffect(() => {
    load();
  }, [load]);

  const dirty = doc != null && text !== doc.content;
  useEffect(() => {
    onDirtyChange?.(dirty);
  }, [dirty, onDirtyChange]);
  useEffect(() => () => onDirtyChange?.(false), [onDirtyChange]);

  const save = useCallback(async () => {
    const current = docRef.current;
    if (savingRef.current || !current || !current.editable) return;
    if (textRef.current === current.content) return;
    savingRef.current = true;
    setSaving(true);
    setSaveError(null);

    const commit = async (expected: string) => {
      const content = textRef.current;
      const sha = await invoke<string>("write_file_content", {
        path,
        content,
        expectedSha256: expected,
      });
      shaRef.current = sha;
      setDoc((d) => (d ? { ...d, content, sha256: sha } : d));
      setSavedFlash(true);
      window.setTimeout(() => setSavedFlash(false), 1600);
      onSaved?.();
    };

    try {
      try {
        await commit(shaRef.current);
      } catch (e) {
        const message = String(e);
        if (!message.includes("changed on disk")) throw e;
        // The agent (or something else) rewrote the file mid-edit.
        const overwrite = await confirm(
          `${filename} changed on disk while you were editing.\n\n` +
            "OK — overwrite it with your version.\n" +
            "Cancel — keep editing without saving.",
          { title: "File changed on disk" },
        );
        if (!overwrite) {
          setSaveError("Not saved — the file changed on disk.");
          return;
        }
        const fresh = await invoke<FileDocument>("read_file_content", { path });
        await commit(fresh.sha256);
      }
    } catch (e) {
      setSaveError(String(e));
    } finally {
      savingRef.current = false;
      setSaving(false);
    }
  }, [path, filename, onSaved]);

  const saveRef = useRef(save);
  saveRef.current = save;

  const extensions = useMemo(
    () => [
      ...languageFor(filename),
      appEditorTheme(theme),
      keymap.of([
        {
          key: "Mod-s",
          run: () => {
            saveRef.current();
            return true;
          },
        },
      ]),
    ],
    [filename, theme],
  );

  const lineCount = text === "" ? 0 : text.split("\n").length;
  const byteSize = new TextEncoder().encode(text).length;

  return (
    <div className="file-preview">
      <div className="file-preview-header">
        <div className="file-breadcrumb" title={path} aria-label={path}>
          {breadcrumbAncestors.length > 0 && (
            <>
              <span className="breadcrumb-ancestors">
                <span className="breadcrumb-ancestor-track">
                  {breadcrumbAncestors.map((part, index) => (
                    <span className="breadcrumb-segment" key={`${part}-${index}`}>
                      <Icon name="chevron-right" size={13} className="breadcrumb-chevron" />
                      <span>{part}</span>
                    </span>
                  ))}
                </span>
              </span>
              <Icon name="chevron-right" size={13} className="breadcrumb-chevron breadcrumb-file-chevron" />
            </>
          )}
          <span className="breadcrumb-name">{filename}</span>
          {dirty && <span className="editor-dirty" title="Unsaved changes" />}
        </div>
        <div className="file-meta editor-actions">
          {isMarkdown && doc != null && (
            <div className="mode-toggle" role="tablist" aria-label="View mode">
              <button
                className={`mode-toggle-btn${mode === "preview" ? " active" : ""}`}
                onClick={() => setMode("preview")}
                role="tab"
                aria-selected={mode === "preview"}
              >
                Preview
              </button>
              <button
                className={`mode-toggle-btn${mode === "edit" ? " active" : ""}`}
                onClick={() => setMode("edit")}
                role="tab"
                aria-selected={mode === "edit"}
              >
                Edit
              </button>
            </div>
          )}
          {doc != null && !doc.editable && (
            <span className="editor-readonly" title={doc.reason ?? undefined}>
              read-only
            </span>
          )}
          {saveError && (
            <span className="editor-error" title={saveError}>
              {saveError}
            </span>
          )}
          {savedFlash && !dirty && <span className="editor-saved">✓ saved</span>}
          {dirty ? (
            <>
              <button
                className="editor-btn"
                onClick={() => setText(doc!.content)}
                disabled={saving}
              >
                Discard
              </button>
              <button
                className="editor-btn editor-btn-primary"
                onClick={() => save()}
                disabled={saving}
                title="Save (⌘S)"
              >
                {saving ? "Saving…" : "Save"}
              </button>
            </>
          ) : (
            doc != null && (
              <>
                <span>{lineCount} lines</span>
                <span className="meta-sep">·</span>
                <span>{formatSize(byteSize)}</span>
                <span className="meta-sep">·</span>
                <span>{detectLang(filename)}</span>
              </>
            )
          )}
        </div>
      </div>

      <div className="file-editor-body">
        {loadError ? (
          <div className="file-error">{loadError}</div>
        ) : doc == null ? (
          <div className="file-loading">Loading…</div>
        ) : isMarkdown && mode === "preview" ? (
          // Renders the buffer (not the disk state), so unsaved edits show
          // up when toggling back from Edit. Raw HTML is not rendered.
          <div className="markdown-preview">
            <div className="markdown-preview-content">
              <ReactMarkdown remarkPlugins={[remarkGfm]}>{text}</ReactMarkdown>
            </div>
          </div>
        ) : (
          <CodeMirror
            className="file-code-editor"
            value={text}
            onChange={setText}
            readOnly={!doc.editable}
            theme={theme === "dark" ? oneDark : "light"}
            height="100%"
            extensions={extensions}
            basicSetup={{
              foldGutter: false,
              highlightActiveLine: true,
              highlightActiveLineGutter: true,
            }}
          />
        )}
      </div>
    </div>
  );
}
