import type { SVGProps } from "react";

type IconName =
  | "activity"
  | "alert-triangle"
  | "ban"
  | "chevron-down"
  | "chevron-right"
  | "cloud"
  | "computer"
  | "check-circle"
  | "download"
  | "drive"
  | "file"
  | "folder"
  | "link"
  | "more"
  | "pause"
  | "play"
  | "plus"
  | "refresh"
  | "settings"
  | "trash"
  | "upload"
  | "x";

interface IconProps extends SVGProps<SVGSVGElement> {
  name: IconName;
  size?: number;
}

export default function Icon({ name, size = 14, ...props }: IconProps) {
  return (
    <svg
      aria-hidden="true"
      fill="none"
      height={size}
      stroke="currentColor"
      strokeLinecap="round"
      strokeLinejoin="round"
      strokeWidth="1.7"
      viewBox="0 0 24 24"
      width={size}
      {...props}
    >
      {name === "activity" && (
        <>
          <path d="M4 19V5" />
          <path d="M20 19V5" />
          <path d="M8 17V9" />
          <path d="M12 17V7" />
          <path d="M16 17v-5" />
        </>
      )}
      {name === "alert-triangle" && (
        <>
          <path d="M10.3 3.7 2.5 17.2A2 2 0 0 0 4.2 20h15.6a2 2 0 0 0 1.7-2.8L13.7 3.7a2 2 0 0 0-3.4 0Z" />
          <path d="M12 9v4" />
          <path d="M12 17h.01" />
        </>
      )}
      {name === "ban" && (
        <>
          <circle cx="12" cy="12" r="9" />
          <path d="m5.7 5.7 12.6 12.6" />
        </>
      )}
      {name === "cloud" && (
        <>
          <path d="M17.5 19H9a7 7 0 1 1 6.71-9h1.79a4.5 4.5 0 1 1 0 9Z" />
        </>
      )}
      {name === "computer" && (
        <>
          <rect x="2" y="3" width="20" height="14" rx="2" />
          <path d="M8 21h8" />
          <path d="M12 17v4" />
        </>
      )}
      {name === "check-circle" && (
        <>
          <circle cx="12" cy="12" r="9" />
          <path d="m8 12 2.5 2.5L16 9" />
        </>
      )}
      {name === "chevron-down" && <path d="m6 9 6 6 6-6" />}
      {name === "chevron-right" && <path d="m9 6 6 6-6 6" />}
      {name === "link" && (
        <>
          <path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71" />
          <path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71" />
        </>
      )}
      {name === "download" && (
        <>
          <path d="M12 3v12" />
          <path d="m7 10 5 5 5-5" />
          <path d="M5 21h14" />
        </>
      )}
      {name === "drive" && (
        <>
          <rect x="3" y="4" width="18" height="16" rx="2" />
          <path d="M3 15h18" />
          <path d="M7 18h.01" />
          <path d="M11 18h.01" />
        </>
      )}
      {name === "file" && (
        <>
          <path d="M14 2H7a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V7Z" />
          <path d="M14 2v5h5" />
        </>
      )}
      {name === "folder" && (
        <>
          <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z" />
          <path d="M3 10h18" />
        </>
      )}
      {name === "more" && (
        <>
          <path d="M12 12h.01" />
          <path d="M17 12h.01" />
          <path d="M7 12h.01" />
        </>
      )}
      {name === "pause" && (
        <>
          <path d="M9 5v14" />
          <path d="M15 5v14" />
        </>
      )}
      {name === "play" && <path d="m8 5 11 7-11 7Z" />}
      {name === "plus" && (
        <>
          <path d="M12 5v14" />
          <path d="M5 12h14" />
        </>
      )}
      {name === "refresh" && (
        <>
          <path d="M20 11a8 8 0 0 0-13.5-5.8L4 8" />
          <path d="M4 4v4h4" />
          <path d="M4 13a8 8 0 0 0 13.5 5.8L20 16" />
          <path d="M16 16h4v4" />
        </>
      )}
      {name === "settings" && (
        <>
          <path d="M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7Z" />
          <path d="M19.4 15a1.7 1.7 0 0 0 .34 1.88l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06A1.7 1.7 0 0 0 15 19.4a1.7 1.7 0 0 0-1 .6 1.7 1.7 0 0 0-.4 1v.2a2 2 0 1 1-4 0V21a1.7 1.7 0 0 0-1.4-1.67 1.7 1.7 0 0 0-1.2.38l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.7 1.7 0 0 0 4.6 15a1.7 1.7 0 0 0-.6-1 1.7 1.7 0 0 0-1-.4h-.2a2 2 0 1 1 0-4H3a1.7 1.7 0 0 0 1.67-1.4 1.7 1.7 0 0 0-.38-1.2l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.7 1.7 0 0 0 9 4.6a1.7 1.7 0 0 0 1-.6 1.7 1.7 0 0 0 .4-1v-.2a2 2 0 1 1 4 0V3a1.7 1.7 0 0 0 1.4 1.67 1.7 1.7 0 0 0 1.2-.38l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.7 1.7 0 0 0 19.4 9c.2.35.53.55 1 .6h.2a2 2 0 1 1 0 4h-.2a1.7 1.7 0 0 0-1 .4 1.7 1.7 0 0 0-.6 1Z" />
        </>
      )}
      {name === "trash" && (
        <>
          <path d="M4 7h16" />
          <path d="M9 7V4h6v3" />
          <path d="m6 7 1 14h10l1-14" />
          <path d="M10 11v6" />
          <path d="M14 11v6" />
        </>
      )}
      {name === "upload" && (
        <>
          <path d="M12 21V9" />
          <path d="m7 14 5-5 5 5" />
          <path d="M5 3h14" />
        </>
      )}
      {name === "x" && (
        <>
          <path d="M18 6 6 18" />
          <path d="m6 6 12 12" />
        </>
      )}
    </svg>
  );
}
