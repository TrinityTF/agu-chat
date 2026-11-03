// Small shared class strings for the chat view (the site's bits.tsx equivalents, on the theme tokens).
export const inputCls =
  "rounded border border-line bg-panel px-2 py-1 text-xs text-text placeholder:text-faint outline-none focus:border-accent/60";
export const btnPrimary =
  "rounded bg-accent px-2.5 py-1 text-xs font-semibold text-white hover:bg-accent-strong disabled:opacity-40 disabled:hover:bg-accent";
export const btnGhost =
  "rounded border border-line px-2.5 py-1 text-xs text-muted hover:border-white/25 hover:text-text disabled:opacity-40";
export const btnDanger =
  "rounded border border-red-500/40 bg-red-500/10 px-2.5 py-1 text-xs font-semibold text-red-300 hover:bg-red-500/20 disabled:opacity-40";

export function fmtTime(ts: number, seconds: boolean): string {
  return new Date(ts).toLocaleTimeString([], seconds ? { hour: "2-digit", minute: "2-digit", second: "2-digit" } : { hour: "2-digit", minute: "2-digit" });
}
