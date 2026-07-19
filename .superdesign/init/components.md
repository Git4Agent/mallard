# Shared UI Components

Mallard uses custom React/CSS primitives rather than a component library. Native buttons, selects, inputs, and semantic sections are styled by `src/App.css`.

## Outline Icon

- Path: `src/components/Icons.tsx`
- Description: dependency-free outline SVG icon set used throughout the desktop shell.
- Props: `name`, `size`, and normal SVG props.

```tsx
import type { SVGProps } from "react";

type IconName =
  | "activity" | "alert-triangle" | "ban" | "chevron-down" | "chevron-right"
  | "cloud" | "computer" | "check-circle" | "download" | "drive" | "file"
  | "folder" | "link" | "more" | "pause" | "play" | "plus" | "refresh"
  | "settings" | "trash" | "upload" | "x";

interface IconProps extends SVGProps<SVGSVGElement> { name: IconName; size?: number; }

export default function Icon({ name, size = 14, ...props }: IconProps) {
  return (
    <svg aria-hidden="true" fill="none" height={size} stroke="currentColor"
      strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.7"
      viewBox="0 0 24 24" width={size} {...props}>
      {name === "folder" && <><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z"/><path d="M3 10h18"/></>}
      {name === "refresh" && <><path d="M20 11a8 8 0 0 0-13.5-5.8L4 8"/><path d="M4 4v4h4"/><path d="M4 13a8 8 0 0 0 13.5 5.8L20 16"/><path d="M16 16h4v4"/></>}
      {name === "settings" && <><path d="M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7Z"/><circle cx="12" cy="12" r="9"/></>}
      {name === "x" && <><path d="M18 6 6 18"/><path d="m6 6 12 12"/></>}
    </svg>
  );
}
```

## Model display helpers

- Path: `src/components/project-sync/model.ts`
- Description: presentation helpers shared by project pages.

```ts
export function projectLabel(project: { display_name: string; local_alias?: string | null }): string {
  return project.local_alias?.trim() || project.display_name;
}

export function compactProjectPath(value: string): string {
  return value.replace(/^\/Users\/[^/]+/, "~");
}
```
