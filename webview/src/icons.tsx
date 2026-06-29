import type { SVGProps } from "react";

type IconName =
  | "plus"
  | "history"
  | "window"
  | "refresh"
  | "settings"
  | "close"
  | "stop"
  | "trash"
  | "caret"
  | "check"
  | "repo"
  | "folder"
  | "github"
  | "agent"
  | "shield"
  | "eye"
  | "bolt"
  | "sliders"
  | "send"
  | "queue"
  | "search"
  | "lock"
  | "up"
  | "down"
  | "paperclip"
  | "cube"
  | "cubeOff"
  | "clock"
  | "terminal"
  | "alert";

const PATHS: Record<IconName, string> = {
  plus: "M8 2.5v11M2.5 8h11",
  history:
    "M8 4v4l2.5 1.5M2.6 9a5.5 5.5 0 1 0 1.2-4.3M3 3v2.4h2.4",
  window: "M2 3.5h12v9H2zM2 6h12",
  refresh: "M12.5 5A5 5 0 1 0 13 9M13 3v2.4h-2.4",
  settings:
    "M6.3 2.4 6 4a4.6 4.6 0 0 0-1.2.7l-1.5-.6-1.3 2.2 1.3 1a4.7 4.7 0 0 0 0 1.4l-1.3 1 1.3 2.2 1.5-.6q.55.43 1.2.7l.3 1.6h2.5l.3-1.6q.65-.27 1.2-.7l1.5.6 1.3-2.2-1.3-1a4.7 4.7 0 0 0 0-1.4l1.3-1-1.3-2.2-1.5.6A4.6 4.6 0 0 0 10 4l-.3-1.6zM8 6.2A1.8 1.8 0 1 1 8 9.8a1.8 1.8 0 0 1 0-3.6z",
  close: "M3.5 3.5l9 9M12.5 3.5l-9 9",
  stop: "M4 4h8v8H4z",
  trash: "M3 4.5h10M6.5 4.5V3h3v1.5M4.5 4.5l.6 8.5h5.8l.6-8.5",
  caret: "M3.5 6l4.5 4.5L12.5 6",
  check: "M3 8.5l3.2 3.2L13 5",
  repo: "M3 3.2h7.2L13 6v6.8H3zM10 3.2V6h3",
  folder: "M2 4.2h4l1.3 1.4H14v6.2H2z",
  github:
    "M8 1.6a6.4 6.4 0 0 0-2 12.5c.3.05.43-.14.43-.3v-1.1c-1.8.4-2.2-.85-2.2-.85-.3-.75-.72-.95-.72-.95-.6-.4.04-.4.04-.4.65.05 1 .67 1 .67.58 1 1.5.72 1.9.55.05-.43.23-.72.4-.88-1.43-.16-2.94-.72-2.94-3.2 0-.7.25-1.3.67-1.74-.07-.16-.3-.83.06-1.73 0 0 .55-.17 1.8.66a6.2 6.2 0 0 1 3.26 0c1.24-.83 1.8-.66 1.8-.66.36.9.13 1.57.06 1.73.42.45.67 1.03.67 1.74 0 2.5-1.52 3.04-2.96 3.2.23.2.44.6.44 1.2v1.8c0 .17.12.36.44.3A6.4 6.4 0 0 0 8 1.6z",
  agent: "M8 2.2 2.6 5.2v5.6L8 13.8l5.4-3V5.2zM8 2.2v5.5m0 0L2.6 5.2m5.4 2.5 5.4-2.5M8 7.7v6.1",
  shield: "M8 2 3 4v4.2c0 3 2.2 5 5 5.8 2.8-.8 5-2.8 5-5.8V4z",
  eye: "M1.6 8S4 3.8 8 3.8 14.4 8 14.4 8 12 12.2 8 12.2 1.6 8 1.6 8zM8 6.1A1.9 1.9 0 1 0 8 9.9 1.9 1.9 0 0 0 8 6.1z",
  bolt: "M8.8 1.8 3.5 8.6h3.3l-.6 5.6 5.3-6.8H8.2z",
  sliders: "M3 4.5h6M11 4.5h2M3 11.5h2M7 11.5h6M9 2.8v3.4M5 9.8v3.4",
  send: "M8 13.2V3.4M3.9 7.5 8 3.2l4.1 4.3",
  queue: "M2.5 4.5h11M2.5 8h7M2.5 11.5h7M12 8l2 3.5h-4z",
  search: "M7 2.5a4.5 4.5 0 1 0 2.9 7.95L13 13.5M7 2.5a4.5 4.5 0 0 1 2.9 7.95",
  lock: "M4.5 7V5.2a3.5 3.5 0 0 1 7 0V7M3.5 7h9v6h-9z",
  up: "M4 9.5 8 5.5l4 4",
  down: "M4 6.5 8 10.5l4-4",
  paperclip: "M12.5 7.2 7.7 12a2.6 2.6 0 0 1-3.7-3.7l4.8-4.8a1.6 1.6 0 0 1 2.3 2.3l-4.8 4.8a.6.6 0 0 1-.9-.9l4.3-4.3",
  cube: "M8 1.8 13.5 4.9v6.2L8 14.2 2.5 11.1V4.9zM2.5 4.9 8 8m0 0 5.5-3.1M8 8v6.2",
  cubeOff: "M8 1.8 13.5 4.9v6.2L8 14.2 2.5 11.1V4.9zM2.5 4.9 8 8m0 0 5.5-3.1M8 8v6.2M2.2 2.2l11.6 11.6",
  clock: "M8 2.6a5.4 5.4 0 1 0 0 10.8 5.4 5.4 0 0 0 0-10.8zM8 5.2V8l2 1.4",
  terminal: "M3 3.5h10v9H3zM5 6.3l2 1.7-2 1.7M8.5 10h2.5",
  alert: "M8 2.5 14.5 13.5h-13zM8 6.5v3.2M8 11.6v.1",
};

const FILLED: Partial<Record<IconName, boolean>> = {
  github: true,
  stop: true,
  bolt: true,
  queue: true,
};

/** The AgentManager mark — same artwork as the activity-bar icon (media/activity.svg),
 *  rendered inline so it inherits theme color instead of the heavy PNG tile. */
export function BrandMark({ className, size = 24 }: { className?: string; size?: number }) {
  return (
    <svg
      className={className}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      aria-hidden="true"
    >
      <path
        d="M12 4.25v5.6M12 14.15v5.6M7.15 16.55l3.85-3.6M13 12.95l3.85 3.6M7.15 7.45 11 11.05M13 11.05l3.85-3.6"
        stroke="currentColor"
        strokeWidth="1.8"
        strokeLinecap="round"
      />
      <circle cx="12" cy="12" r="2.35" fill="currentColor" />
      <circle cx="12" cy="3.5" r="2.2" fill="currentColor" />
      <circle cx="5.75" cy="17.75" r="2.2" fill="currentColor" />
      <circle cx="18.25" cy="17.75" r="2.2" fill="currentColor" />
    </svg>
  );
}

export function Icon({ name, className, ...rest }: { name: IconName; className?: string } & SVGProps<SVGSVGElement>) {
  const filled = FILLED[name];
  return (
    <svg
      className={className}
      width="16"
      height="16"
      viewBox="0 0 16 16"
      fill={filled ? "currentColor" : "none"}
      stroke={filled ? "none" : "currentColor"}
      strokeWidth="1.4"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      {...rest}
    >
      <path d={PATHS[name]} />
    </svg>
  );
}
