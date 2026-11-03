// Ported from modtools.agu.lol web/src/app/pages/Chat.tsx (commit 1b826c2): PickerItem / PickerSection /
// EmotePicker. Changes: restyled on the theme tokens; sections are built by the send box from the pane's
// emote data (Twitch user emotes, 7TV personal / channel / zero-width / global, BTTV, FFZ, Rumble, Kick).
import { useRef, useState } from "react";
import { inputCls } from "../ui";

export type PickerItem = { key: string; name: string; img: string; tooltip: string; insert?: string };
export type PickerSection = { id: string; label: string; provider: "7tv" | "bttv" | "ffz" | "twitch" | "rumble" | "kick"; items: PickerItem[] };

export function EmotePicker({ sections, onPick }: { sections: PickerSection[]; onPick: (name: string) => void }) {
  const [q, setQ] = useState("");
  const sectionRefs = useRef<Map<string, HTMLDivElement>>(new Map());

  const filtered = q
    ? sections.map((s) => ({ ...s, items: s.items.filter((it) => it.name.toLowerCase().includes(q.toLowerCase())) })).filter((s) => s.items.length > 0)
    : sections;

  // Only offer a jump for providers actually present: a tab that scrolls nowhere is worse than no tab.
  const allTabs: { id: PickerSection["provider"]; label: string }[] = [
    { id: "twitch", label: "Twitch" },
    { id: "7tv", label: "7TV" },
    { id: "bttv", label: "BTTV" },
    { id: "ffz", label: "FFZ" },
    { id: "rumble", label: "Rumble" },
    { id: "kick", label: "Kick" },
  ];
  const providerTabs = allTabs.filter((t) => sections.some((s) => s.provider === t.id));

  const scrollToProvider = (p: PickerSection["provider"]) => {
    const first = filtered.find((s) => s.provider === p);
    if (!first) return;
    sectionRefs.current.get(first.id)?.scrollIntoView({ block: "start" });
  };

  return (
    <div className="w-80 rounded border border-line bg-raised p-2 shadow-2xl shadow-black/60">
      <input autoFocus className={`${inputCls} mb-1.5 w-full`} placeholder="Search emotes..." value={q} onChange={(e) => setQ(e.target.value)} />
      <div className="mb-1.5 flex flex-wrap gap-1">
        {providerTabs.map((p) => (
          <button
            key={p.id}
            type="button"
            onClick={() => scrollToProvider(p.id)}
            className="rounded border border-line px-1.5 py-px text-[11px] font-medium text-muted hover:border-white/25 hover:text-text"
          >
            {p.label}
          </button>
        ))}
      </div>
      <div className="max-h-64 overflow-y-auto">
        {filtered.length === 0 && <p className="py-6 text-center text-xs text-faint">No emotes found.</p>}
        {filtered.map((s) => (
          <div
            key={s.id}
            ref={(el) => {
              if (el) sectionRefs.current.set(s.id, el);
              else sectionRefs.current.delete(s.id);
            }}
          >
            <div className="mb-1 text-[10px] font-semibold uppercase tracking-wide text-faint">{s.label}</div>
            <div className="mb-2 grid grid-cols-8 gap-0.5">
              {s.items.map((it) => (
                <button key={it.key} type="button" title={it.tooltip} onClick={() => onPick(it.insert ?? it.name)} className="rounded p-1 hover:bg-raised-2">
                  <img src={it.img} alt={it.name} loading="lazy" decoding="async" className="h-6 w-6 object-contain" />
                </button>
              ))}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
