// Platform brand marks, copied from modtools.agu.lol web/src/ui.tsx (commit 1b826c2).
import type { CSSProperties } from "react";
import type { Platform } from "./protocol";

export function TwitchIcon({ className = "h-4 w-4" }: { className?: string }) {
  return (
    <svg viewBox="0 0 24 24" fill="currentColor" className={className} aria-hidden>
      <path d="M4.3 1 1.7 4.5v15.2h5.2V23h3l3.3-3.3h4.4L23 14.4V1H4.3zm16.6 12.5-3 3h-4.8l-3.2 3.2v-3.2H5.7V3.1h15.2v10.4zM17 6.5h-2.1v5.2H17V6.5zm-5.6 0H9.3v5.2h2.1V6.5z" />
    </svg>
  );
}

export function RumbleIcon({ className = "h-4 w-4" }: { className?: string }) {
  return (
    <svg viewBox="0 0 32 34" fill="currentColor" className={className} aria-hidden>
      <path d="M19.1 19a2.5 2.5 0 0 0 0-4c-2-1.5-4-2.8-6.2-3.7-1.5-.7-3.2.2-3.4 1.9-.4 2.5-.5 5.1-.2 7.5a2.4 2.4 0 0 0 3.4 2 28.3 28.3 0 0 0 6.4-3.6Zm10-7.3a7.8 7.8 0 0 1 0 10.8c-5.2 5.3-11.6 9-18.8 10.7a7.3 7.3 0 0 1-8.8-5A39.6 39.6 0 0 1 1.7 6 7.3 7.3 0 0 1 10.3.7c7 1.6 13.7 5.7 18.7 11Z" />
    </svg>
  );
}

// YouTube's play button. Kept as a single path on `currentColor` like the
// others so it can sit muted in a header or brand-red where that reads better.
export function YouTubeIcon({ className = "h-4 w-4", style }: { className?: string; style?: CSSProperties }) {
  return (
    <svg viewBox="0 0 24 24" fill="currentColor" className={className} style={style} aria-hidden>
      <path d="M23.5 6.2a3 3 0 0 0-2.1-2.1C19.5 3.6 12 3.6 12 3.6s-7.5 0-9.4.5A3 3 0 0 0 .5 6.2C0 8.1 0 12 0 12s0 3.9.5 5.8a3 3 0 0 0 2.1 2.1c1.9.5 9.4.5 9.4.5s7.5 0 9.4-.5a3 3 0 0 0 2.1-2.1c.5-1.9.5-5.8.5-5.8s0-3.9-.5-5.8zM9.5 15.6V8.4l6.3 3.6-6.3 3.6z" />
    </svg>
  );
}

// X's mark. One path on `currentColor` like the rest, so it reads as a muted
// header glyph or as brand white depending on where it sits — X has no brand
// colour beyond black and white, so the column uses plain white rather than
// inventing one.
export function XIcon({ className = "h-4 w-4", style }: { className?: string; style?: CSSProperties }) {
  return (
    <svg viewBox="0 0 24 24" fill="currentColor" className={className} style={style} aria-hidden>
      <path d="M18.244 2.25h3.308l-7.227 8.26 8.502 11.24H16.17l-5.214-6.817L4.99 21.75H1.68l7.73-8.835L1.254 2.25H8.08l4.713 6.231zm-1.161 17.52h1.833L7.084 4.126H5.117z" />
    </svg>
  );
}

// Kick's angular blocked "K". `currentColor` like the others, so it can sit in
// a muted header as well as in brand green — pass a colour via className or
// style where the brand mark is wanted (KickChat.tsx's KICK_GREEN).
//
// `style` is accepted because Kick's green is a hex value with no Tailwind
// token behind it, and an arbitrary-value class for one colour used in a
// handful of places is worse than passing it.
export function KickIcon({ className = "h-4 w-4", style }: { className?: string; style?: CSSProperties }) {
  return (
    <svg viewBox="0 0 24 24" fill="currentColor" className={className} style={style} aria-hidden>
      <path d="M1.333 0h8v5.333H12V2.667h2.667V0h8v8H20v2.667h-2.667v2.666H20V16h2.667v8h-8v-2.667H12v-2.666H9.333V24h-8Z" />
    </svg>
  );
}

export const PLATFORM_COLOR: Record<Platform, string> = {
  twitch: "#a970ff",
  kick: "#53fc18",
  youtube: "#ff0033",
  rumble: "#85c742",
  x: "#ffffff",
};

export const PLATFORM_LABEL: Record<Platform, string> = {
  twitch: "Twitch",
  kick: "Kick",
  youtube: "YouTube",
  rumble: "Rumble",
  x: "X",
};

export function PlatformIcon({ platform, className = "h-3.5 w-3.5", colored = true }: { platform: Platform; className?: string; colored?: boolean }) {
  const style = colored ? { color: PLATFORM_COLOR[platform] } : undefined;
  switch (platform) {
    case "twitch":
      return <span style={style} className="inline-flex"><TwitchIcon className={className} /></span>;
    case "kick":
      return <KickIcon className={className} style={style} />;
    case "youtube":
      return <YouTubeIcon className={className} style={style} />;
    case "rumble":
      return <span style={style} className="inline-flex"><RumbleIcon className={className} /></span>;
    case "x":
      return <XIcon className={className} style={style} />;
  }
}
