// Ported from modtools.agu.lol web/src/app/pages/RumbleChat.tsx, KickChat.tsx, YouTubeChat.tsx and
// XChat.tsx (commit 1b826c2): RumbleRow (+ badges, rants, :shortcode: emotes), KickRow (+ badge chips,
// [emote:id:name], reply chip), YouTubeRow (parts + Super Chats, member tag) and XRow, with their text
// helpers. Changes: one dense RowFrame shell for all four, display options come from the app's Settings
// instead of per-platform blocks (no avatars: Chatterino rows have none), memoized, mod buttons via
// handlers (modMode + the account's caps), clicking a name opens the pane's user card.
import { memo, type ReactNode } from "react";
import { CheckIcon, FlagIcon, GearIcon, GiftIcon, StarIcon, SwordIcon } from "../../icons";
import { TextRun } from "../chatText";
import { emoteTip } from "../emoteTip";
import { emoteBox } from "../parts";
import { readableColor } from "../twitch/colors";
import type {
  KickBadge,
  KickMsg,
  Row,
  RumbleBadgeDefs,
  RumbleEmotes,
  RumbleMsg,
  XMsg,
  YouTubeBadge,
  YouTubeMsg,
  YouTubePart,
} from "../types";
import type { CoEnv, RowCtx } from "./env";
import { HoverButtons, ModButtons, RowFrame } from "./RowFrame";

const nameColorOf = (c: string | undefined, readable: boolean) => (c ? (readable ? readableColor(c) : c) : "#a1a1aa");
const nameCls = "font-semibold hover:underline";

// ---------------------------------------------------------------------------
// Rumble
// ---------------------------------------------------------------------------

function badgeIcon(def: RumbleBadgeDefs[string]): string | null {
  const icons = def.icons || {};
  // These render at ~15px, so the 96 art is sharper where it exists and the 48 is the only option elsewhere.
  const path = icons["96"] || icons["48"] || Object.values(icons)[0];
  if (!path) return null;
  return path.startsWith("http") ? path : `https://rumble.com${path}`;
}

function RumbleBadges({ badges, defs }: { badges: string[]; defs: RumbleBadgeDefs }) {
  const known = badges.map((id) => [id, defs[id]] as const).filter(([, def]) => !!def && !!badgeIcon(def));
  if (!known.length) return null;
  return (
    <>
      {known.map(([id, def]) => (
        <img
          key={id}
          src={badgeIcon(def) as string}
          alt={(def.label && def.label.en) || id}
          title={(def.label && def.label.en) || id}
          loading="lazy"
          className="mr-1 inline-block h-[1.1em] w-[1.1em] shrink-0 align-[-0.2em]"
        />
      ))}
    </>
  );
}

// Rants (Rumble's paid highlighted messages) are tiered by price: approximating the tiers keeps a $50 rant
// louder than a $1 one.
function rantTone(cents: number): string {
  const d = cents / 100;
  if (d >= 100) return "border-rose-500/50 bg-rose-500/15";
  if (d >= 50) return "border-orange-500/50 bg-orange-500/15";
  if (d >= 20) return "border-amber-500/50 bg-amber-500/15";
  if (d >= 10) return "border-yellow-500/50 bg-yellow-500/15";
  if (d >= 5) return "border-lime-500/50 bg-lime-500/15";
  if (d >= 2) return "border-emerald-500/50 bg-emerald-500/15";
  return "border-sky-500/50 bg-sky-500/15";
}

const money = (cents: number) => {
  const d = cents / 100;
  return "$" + (Number.isInteger(d) ? String(d) : d.toFixed(2));
};

// Rumble emotes are `:name:` tokens (":r+cooldoge:") resolved against the chat's own emote list. A token
// with no emote is left exactly as typed: people write colons in ordinary sentences.
type RumblePart = { kind: "text"; text: string } | { kind: "emote"; name: string; file: string };
const RUMBLE_TOKEN = /:([A-Za-z0-9_+.-]{1,60}):/g;

export function splitRumbleEmotes(text: string, emotes: RumbleEmotes): RumblePart[] {
  if (!text) return [];
  if (!emotes || !Object.keys(emotes).length) return [{ kind: "text", text }];
  const parts: RumblePart[] = [];
  let last = 0;
  RUMBLE_TOKEN.lastIndex = 0;
  let m: RegExpExecArray | null;
  while ((m = RUMBLE_TOKEN.exec(text))) {
    const hit = emotes[m[1]];
    if (!hit) continue;
    if (m.index > last) parts.push({ kind: "text", text: text.slice(last, m.index) });
    parts.push({ kind: "emote", name: m[1], file: hit.file });
    last = m.index + m[0].length;
  }
  if (!parts.length) return [{ kind: "text", text }];
  if (last < text.length) parts.push({ kind: "text", text: text.slice(last) });
  return parts;
}

function EmoteImg({ src, alt, tipName, tipLabel, ctx }: { src: string; alt: string; tipName: string; tipLabel: string; ctx: RowCtx }) {
  const { px, maxW, vMargin } = emoteBox(ctx.rs.fontSize, ctx.rs.emoteSize);
  return (
    <img
      src={src}
      alt={alt}
      loading="lazy"
      decoding="async"
      className="mx-px inline-block object-contain align-middle"
      style={{ maxHeight: px, maxWidth: maxW, margin: `${vMargin}px 1px` }}
      onMouseMove={(e) => emoteTip.show(e.clientX, e.clientY, { img: src, name: tipName, label: tipLabel })}
      onMouseLeave={() => emoteTip.hide()}
    />
  );
}

function RumbleText({ text, emotes, ctx }: { text: string; emotes: RumbleEmotes; ctx: RowCtx }) {
  return (
    <>
      {splitRumbleEmotes(text, emotes).map((p, i) =>
        p.kind === "emote" ? <EmoteImg key={i} src={p.file} alt={`:${p.name}:`} tipName={`:${p.name}:`} tipLabel="Rumble" ctx={ctx} /> : <TextRun key={i} text={p.text} />,
      )}
    </>
  );
}

type RowOf<K extends Row["k"]> = Extract<Row, { k: K }>;

export const RumbleRow = memo(function RumbleRow({ row, ctx, env, modMode }: { row: RowOf<"rumble">; ctx: RowCtx; env: CoEnv; modMode: boolean }) {
  const { rs, h } = ctx;
  const msg: RumbleMsg = row.m;
  if (msg.deleted && rs.deletedMessages === "hide") return null;
  const rant = msg.rant && msg.rant.cents > 0 ? msg.rant : null;
  const striking = msg.deleted && rs.deletedMessages === "strike";
  const canMod = env.caps.canModerate && !msg.deleted;
  const mod =
    modMode && canMod ? (
      <ModButtons
        timeoutTitle="Time out 5 minutes"
        onBan={() => h.rumbleMod(row.sk, "ban", msg)}
        onTimeout={() => h.rumbleMod(row.sk, "timeout", msg)}
        onDelete={() => h.rumbleMod(row.sk, "delete", msg)}
      />
    ) : null;
  return (
    <RowFrame
      ctx={ctx}
      n={row.ord}
      ts={msg.ts}
      platform="rumble"
      className={rant ? `my-0.5 border ${rantTone(rant.cents)}` : ""}
      mod={mod}
      hover={<HoverButtons onCopy={() => h.copy(msg.text)} onDelete={canMod ? () => h.rumbleMod(row.sk, "delete", msg) : undefined} />}
    >
      {rs.showBadges && <RumbleBadges badges={msg.badges} defs={env.rumbleBadges} />}
      <button
        type="button"
        className={nameCls}
        style={{ color: nameColorOf(msg.user.color, rs.readableColors) }}
        onClick={(e) => h.openCard(e.currentTarget, row.sk, { id: msg.user.id, login: msg.user.login, name: msg.user.name, avatar: msg.user.avatar, color: msg.user.color })}
      >
        {msg.user.name || "unknown"}
      </button>
      {/* A message posted as a channel still shows who is behind it, since that is who a mod would act on. */}
      {msg.channel && msg.user.login && msg.channel.name !== msg.user.login && <span className="ml-1 text-[10px] text-faint">({msg.user.login})</span>}
      <span className="text-muted">: </span>
      {rant && <span className="mr-1 rounded bg-black/30 px-1 text-[11px] font-bold text-white">{money(rant.cents)}</span>}
      <span className={striking ? "text-faint line-through" : "text-text"}>
        <RumbleText text={msg.text} emotes={env.rumbleEmotes} ctx={ctx} />
      </span>
    </RowFrame>
  );
});

// ---------------------------------------------------------------------------
// Kick
// ---------------------------------------------------------------------------

// Everything but a channel's subscriber badges ships without artwork (Kick draws them as SVG in its own
// JS), so those stay chips carrying the badge's colour and a glyph; an unknown type renders as its label.
const KICK_BADGE_STYLE: Record<string, { glyph: ReactNode; className: string }> = {
  broadcaster: { glyph: "◉", className: "bg-red-500/20 text-red-300" },
  moderator: { glyph: <SwordIcon className="h-3 w-3" />, className: "bg-sky-500/20 text-sky-300" },
  verified: { glyph: <CheckIcon className="h-3 w-3" />, className: "bg-blue-500/20 text-blue-300" },
  vip: { glyph: "◆", className: "bg-fuchsia-500/20 text-fuchsia-300" },
  og: { glyph: <StarIcon className="h-3 w-3" />, className: "bg-emerald-500/20 text-emerald-300" },
  founder: { glyph: <FlagIcon className="h-3 w-3" />, className: "bg-amber-500/20 text-amber-300" },
  subscriber: { glyph: <StarIcon className="h-3 w-3" />, className: "bg-lime-500/20 text-lime-300" },
  sub_gifter: { glyph: <GiftIcon className="h-3 w-3" />, className: "bg-purple-500/20 text-purple-300" },
  staff: { glyph: <GearIcon className="h-3 w-3" />, className: "bg-zinc-400/20 text-zinc-200" },
  trainee: { glyph: "◐", className: "bg-teal-500/20 text-teal-300" },
  bot: { glyph: "⌬", className: "bg-zinc-500/20 text-zinc-300" },
};

function KickBadges({ badges }: { badges: KickBadge[] }) {
  if (!badges.length) return null;
  return (
    <>
      {badges.map((b, i) => {
        const style = KICK_BADGE_STYLE[b.type];
        const label = b.count > 0 ? `${b.text || b.type} (${b.count})` : b.text || b.type;
        if (b.url)
          return (
            <img key={`${b.type}-${i}`} src={b.url} alt={label} title={label} loading="lazy" className="mr-1 inline-block h-[1.1em] w-[1.1em] rounded-[2px] object-contain align-[-0.2em]" />
          );
        return (
          <span
            key={`${b.type}-${i}`}
            title={label}
            className={`mr-1 inline-flex h-[1.1em] min-w-[1.1em] items-center justify-center rounded px-[3px] align-[-0.2em] text-[10px] font-bold leading-none ${style ? style.className : "bg-white/10 text-muted"}`}
          >
            {style ? style.glyph : (b.text || b.type).slice(0, 3)}
          </span>
        );
      })}
    </>
  );
}

// Kick emotes arrive INSIDE the text as `[emote:1234:name]` and the image is derived from the id alone.
type KickPart = { kind: "text"; text: string } | { kind: "emote"; id: string; name: string };
const KICK_TOKEN = /\[emote:(\d+):([^\]]{1,80})\]/g;
export const kickEmoteUrl = (id: string) => `https://files.kick.com/emotes/${id}/fullsize`;

export function splitKickEmotes(text: string): KickPart[] {
  if (!text) return [];
  const parts: KickPart[] = [];
  let last = 0;
  KICK_TOKEN.lastIndex = 0;
  let m: RegExpExecArray | null;
  while ((m = KICK_TOKEN.exec(text))) {
    if (m.index > last) parts.push({ kind: "text", text: text.slice(last, m.index) });
    parts.push({ kind: "emote", id: m[1], name: m[2] });
    last = m.index + m[0].length;
  }
  if (!parts.length) return [{ kind: "text", text }];
  if (last < text.length) parts.push({ kind: "text", text: text.slice(last) });
  return parts;
}

function KickText({ text, ctx }: { text: string; ctx: RowCtx }) {
  return (
    <>
      {splitKickEmotes(text).map((p, i) =>
        p.kind === "emote" ? <EmoteImg key={i} src={kickEmoteUrl(p.id)} alt={p.name} tipName={p.name} tipLabel="Kick" ctx={ctx} /> : <TextRun key={i} text={p.text} />,
      )}
    </>
  );
}

export const KickRow = memo(function KickRow({ row, ctx, env, modMode }: { row: RowOf<"kick">; ctx: RowCtx; env: CoEnv; modMode: boolean }) {
  const { rs, h } = ctx;
  const msg: KickMsg = row.m;
  if (msg.deleted && rs.deletedMessages === "hide") return null;
  const striking = msg.deleted && rs.deletedMessages === "strike";
  const canMod = env.caps.canModerate && !!msg.user.id && !msg.deleted;
  const canDelete = env.caps.canDelete && !msg.deleted;
  const mod =
    modMode && (canMod || canDelete) ? (
      <ModButtons
        onBan={canMod ? () => h.kickMod(row.sk, "ban", msg) : undefined}
        onTimeout={canMod ? () => h.kickMod(row.sk, "timeout", msg, 600) : undefined}
        onDelete={canDelete ? () => h.kickMod(row.sk, "delete", msg) : undefined}
      />
    ) : null;
  return (
    <RowFrame
      ctx={ctx}
      n={row.ord}
      ts={msg.ts}
      platform="kick"
      mod={mod}
      hover={
        <HoverButtons
          onCopy={() => h.copy(msg.text)}
          onReply={env.canSend && !msg.deleted ? () => h.reply({ sk: row.sk, platform: "kick", id: msg.id, user: msg.user.name || msg.user.login, text: msg.text }) : undefined}
          onDelete={canDelete ? () => h.kickMod(row.sk, "delete", msg) : undefined}
        />
      }
    >
      {rs.showBadges && <KickBadges badges={msg.badges} />}
      <button
        type="button"
        className={nameCls}
        style={{ color: nameColorOf(msg.user.color, rs.readableColors) }}
        onClick={(e) => h.openCard(e.currentTarget, row.sk, { id: msg.user.id, login: msg.user.login, name: msg.user.name, avatar: msg.user.avatar, color: msg.user.color })}
      >
        {msg.user.name || "unknown"}
      </button>
      <span className="text-muted">: </span>
      {/* A reply's parent, so a threaded message isn't a non-sequitur. */}
      {msg.reply && (
        <span className="mr-1 rounded bg-white/5 px-1 text-[10px] text-faint" title={msg.reply.text}>
          ↩ {msg.reply.name}
        </span>
      )}
      <span className={striking ? "text-faint line-through" : msg.deleted && rs.deletedMessages !== "show" ? "italic text-faint" : "text-text"}>
        <KickText text={msg.text} ctx={ctx} />
      </span>
    </RowFrame>
  );
});

// ---------------------------------------------------------------------------
// YouTube
// ---------------------------------------------------------------------------

// Members ship real artwork (an image); moderator / owner / verified ship an icon type with no image (a chip).
const YT_BADGE_STYLE: Record<string, { glyph: ReactNode; className: string }> = {
  OWNER: { glyph: "◉", className: "bg-amber-500/20 text-amber-300" },
  MODERATOR: { glyph: <SwordIcon className="h-3 w-3" />, className: "bg-sky-500/20 text-sky-300" },
  VERIFIED: { glyph: <CheckIcon className="h-3 w-3" />, className: "bg-zinc-400/20 text-zinc-200" },
};

function YouTubeBadges({ badges }: { badges: YouTubeBadge[] }) {
  if (!badges.length) return null;
  return (
    <>
      {badges.map((b, i) =>
        b.url ? (
          <img key={i} src={b.url} alt={b.label} title={b.label} loading="lazy" className="mr-1 inline-block h-[1.1em] w-[1.1em] shrink-0 align-[-0.2em]" />
        ) : (
          <span
            key={i}
            title={b.label}
            className={`mr-1 inline-flex h-[1.1em] min-w-[1.1em] items-center justify-center rounded px-[3px] align-[-0.2em] text-[10px] font-bold leading-none ${YT_BADGE_STYLE[b.icon] ? YT_BADGE_STYLE[b.icon].className : "bg-white/10 text-muted"}`}
          >
            {YT_BADGE_STYLE[b.icon] ? YT_BADGE_STYLE[b.icon].glyph : (b.label || b.icon).slice(0, 3)}
          </span>
        ),
      )}
    </>
  );
}

// Emoji arrive with their URLs already attached: a straight render with no lookup table.
function YouTubeText({ parts, ctx }: { parts: YouTubePart[]; ctx: RowCtx }) {
  return (
    <>
      {parts.map((p, i) =>
        p.t === "emoji" ? <EmoteImg key={i} src={p.u} alt={p.a} tipName={p.a} tipLabel={p.custom ? "YouTube member emote" : "YouTube"} ctx={ctx} /> : <span key={i}>{p.v}</span>,
      )}
    </>
  );
}

export const YouTubeRow = memo(function YouTubeRow({ row, ctx }: { row: RowOf<"youtube">; ctx: RowCtx }) {
  const { rs, h } = ctx;
  const msg: YouTubeMsg = row.m;
  const paid = msg.paid && msg.paid.amount ? msg.paid : null;
  return (
    <RowFrame
      ctx={ctx}
      n={row.ord}
      ts={msg.ts}
      platform="youtube"
      className={paid ? "my-0.5 border border-white/10" : ""}
      // Super Chats carry YouTube's own tier colour, which is what the viewer paid for.
      style={paid ? { background: paid.bg || undefined } : undefined}
      hover={<HoverButtons onCopy={() => h.copy(msg.text)} />}
    >
      {rs.showBadges && <YouTubeBadges badges={msg.badges} />}
      <button type="button" className="font-semibold text-text/80 hover:underline" onClick={(e) => h.openCard(e.currentTarget, row.sk, { id: msg.user.id, login: "", name: msg.user.name, avatar: msg.user.avatar })}>
        {msg.user.name || "unknown"}
      </button>
      <span className="text-muted">: </span>
      {paid && <span className="mr-1 rounded bg-black/30 px-1 text-[11px] font-bold text-white">{paid.amount}</span>}
      {msg.notice === "membership" && <span className="mr-1 rounded bg-emerald-500/20 px-1 text-[10px] font-semibold text-emerald-300">member</span>}
      <span className="text-text">
        {paid && paid.sticker && !msg.parts.length ? <span className="italic text-muted">sent a Super Sticker</span> : <YouTubeText parts={msg.parts} ctx={ctx} />}
      </span>
    </RowFrame>
  );
});

// ---------------------------------------------------------------------------
// X: plain text, no emote system (the shared link and mention helpers handle the whole message).
// ---------------------------------------------------------------------------

export const XRow = memo(function XRow({ row, ctx }: { row: RowOf<"x">; ctx: RowCtx }) {
  const { h } = ctx;
  const msg: XMsg = row.m;
  return (
    <RowFrame ctx={ctx} n={row.ord} ts={msg.ts} platform="x" hover={<HoverButtons onCopy={() => h.copy(msg.text)} />}>
      <button
        type="button"
        className="font-semibold text-text/80 hover:underline"
        title={msg.user.login ? "@" + msg.user.login : undefined}
        onClick={(e) => h.openCard(e.currentTarget, row.sk, { id: msg.user.id, login: msg.user.login, name: msg.user.name, avatar: msg.user.avatar })}
      >
        {msg.user.name || "unknown"}
      </button>
      <span className="text-muted">: </span>
      <span className="text-text">
        <TextRun text={msg.text} />
      </span>
    </RowFrame>
  );
});
