// Ported from modtools.agu.lol web/src/app/pages/Chat.tsx (commit 1b826c2): EmotePart, MessageText, Badges
// and the emote sizing helpers. Changes: sizes are derived from the font size (settings.fontSize is px
// here, the site had small/medium/large), the shared tooltip is the module store in emoteTip.tsx, and the
// Twitch badge row uses the denser 16px badge.
import type { CSSProperties, MouseEvent as RMouseEvent } from "react";
import { ChatLink } from "./chatText";
import { emoteTip, type EmoteTipContent } from "./emoteTip";
import type { EmoteSets, ProviderEmote, SizeOpt } from "./twitch/emotes";
import { PROVIDER_LABEL, providerEmoteUrl, providerTooltipUrl } from "./twitch/emotes";
import type { EmoteSource, MsgPart } from "./twitch/parts";
import { BADGE_PX, BADGE_VMARGIN, parseBadgeList, type BadgeMap } from "./twitch/badges";

/** Line height as a multiple of the font size (Chatterino-dense). */
export const LINE_HEIGHT = 1.45;

const EMOTE_SCALE: Record<SizeOpt, number> = { small: 1.45, medium: 1.9, large: 2.5 };
const TWITCH_DENSITY: Record<SizeOpt, "1.0" | "2.0" | "3.0"> = { small: "1.0", medium: "2.0", large: "3.0" };

export const emotePx = (fontPx: number, size: SizeOpt) => Math.round(fontPx * EMOTE_SCALE[size]);

export const twitchEmoteUrl = (id: string, size: SizeOpt) =>
  `https://static-cdn.jtvnw.net/emoticons/v2/${id}/default/dark/${TWITCH_DENSITY[size]}`;

// How wide an emote may get before it is scaled DOWN rather than allowed to keep growing sideways
// (a 4:1 emote at 28px tall is 112px across): Twitch's own chat does the same.
const EMOTE_MAX_ASPECT = 2.4;

// A tall emote image must not inflate the line box (and push the name above the text baseline): a
// negative top/bottom margin of (emote px - line height) / 2, same trick 7TV's extension uses.
export function emoteBox(fontPx: number, size: SizeOpt): { px: number; maxW: number; vMargin: number } {
  const px = emotePx(fontPx, size);
  return { px, maxW: Math.round(px * EMOTE_MAX_ASPECT), vMargin: Math.min(0, -Math.round((px - fontPx * LINE_HEIGHT) / 2)) };
}

function emoteTipContent(source: EmoteSource, id: string, name: string, zeroWidth: boolean, entry: ProviderEmote | null): EmoteTipContent {
  return {
    img: source === "twitch" ? `https://static-cdn.jtvnw.net/emoticons/v2/${id}/default/dark/3.0` : entry ? providerTooltipUrl(entry) : "",
    name,
    label: source === "twitch" ? "Twitch" : PROVIDER_LABEL[source] + (zeroWidth ? " zero-width" : ""),
  };
}

function emotePartUrl(source: EmoteSource, id: string, size: SizeOpt, entry: ProviderEmote | null): string {
  if (source === "twitch") return twitchEmoteUrl(id, size);
  return entry ? providerEmoteUrl(entry, size) : "";
}

function EmotePart({
  part,
  size,
  fontPx,
  emotes,
}: {
  part: Extract<MsgPart, { kind: "emote" }>;
  size: SizeOpt;
  fontPx: number;
  emotes: EmoteSets | null;
}) {
  const { px, maxW, vMargin } = emoteBox(fontPx, size);
  // maxHeight/maxWidth with auto dimensions: whichever limit binds first wins - height for a square
  // emote, width for a wide one.
  const fit: CSSProperties = { maxHeight: px, maxWidth: maxW, height: "auto", width: "auto", verticalAlign: "middle" };
  const entry = part.source === "twitch" ? null : emotes?.all.get(part.name) || null;
  const src = emotePartUrl(part.source, part.id, size, entry);
  return (
    <span className="relative inline-flex items-center align-middle" style={{ height: px, margin: `${vMargin}px 0` }}>
      <img
        src={src}
        alt={part.name}
        decoding="async"
        style={fit}
        className="inline-block"
        onMouseMove={(e) => emoteTip.show(e.clientX, e.clientY, emoteTipContent(part.source, part.id, part.name, false, entry))}
        onMouseLeave={() => emoteTip.hide()}
      />
      {part.overlays.map((o) => {
        const oEntry = emotes?.all.get(o.name) || null;
        return (
          <img
            key={o.id}
            src={emotePartUrl(o.source, o.id, size, oEntry)}
            alt={o.name}
            decoding="async"
            // Same fit as the base, so a zero-width overlay keeps sitting on top of it.
            style={fit}
            className="absolute inset-0 m-auto inline-block"
            onMouseMove={(e) => emoteTip.show(e.clientX, e.clientY, emoteTipContent(o.source, o.id, o.name, true, oEntry))}
            onMouseLeave={() => emoteTip.hide()}
          />
        );
      })}
    </span>
  );
}

// Mention chips render coloured + bold with the chatter's own name colour; clickable only where the
// caller wires `onMentionClick` (the live message rows).
export function MessageText({
  parts,
  size,
  fontPx,
  emotes,
  onMentionClick,
}: {
  parts: MsgPart[];
  size: SizeOpt;
  fontPx: number;
  emotes: EmoteSets | null;
  onMentionClick?: (e: RMouseEvent<HTMLButtonElement>, who: { userId: string; login: string; name: string }) => void;
}) {
  return (
    <>
      {parts.map((p, i) => {
        if (p.kind === "text") return <span key={i}>{p.text}</span>;
        if (p.kind === "mention") {
          if (onMentionClick) {
            return (
              <button
                key={i}
                type="button"
                className="font-semibold hover:underline"
                style={{ color: p.color || undefined }}
                onClick={(e) => onMentionClick(e, { userId: p.userId, login: p.login, name: p.name })}
              >
                @{p.name}
              </button>
            );
          }
          return (
            <span key={i} className="font-semibold" style={{ color: p.color || undefined }}>
              @{p.name}
            </span>
          );
        }
        if (p.kind === "gif") {
          // Twitch's docs require the URL to be rendered exactly as given (no resizing parameters, no
          // proxy), so the box is constrained with CSS. Block-level: a GIF is a message on its own.
          return <img key={i} src={p.url} alt={p.alt} title={p.alt} loading="lazy" className="my-1 block max-h-40 max-w-full rounded object-contain" />;
        }
        if (p.kind === "link") return <ChatLink key={i} href={p.href} text={p.text} />;
        return <EmotePart key={i} part={p} size={size} fontPx={fontPx} emotes={emotes} />;
      })}
    </>
  );
}

export function Badges({ tag, map }: { tag: string; map: BadgeMap }) {
  const list = parseBadgeList(tag, map);
  if (!list.length) return null;
  return (
    // self-center: a flex item in an items-baseline row would otherwise align on the image baseline.
    <span className="inline-flex items-center gap-[2px] self-center align-middle">
      {list.map((b, i) => (
        <img
          key={i}
          src={b.url}
          title={b.title}
          alt=""
          decoding="async"
          className="inline-block align-middle"
          style={{ height: BADGE_PX, width: BADGE_PX, margin: `${BADGE_VMARGIN}px 0` }}
        />
      ))}
    </span>
  );
}
