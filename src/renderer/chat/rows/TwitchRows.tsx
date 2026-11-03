// Ported from modtools.agu.lol web/src/app/pages/Chat.tsx (commit 1b826c2): MessageRow, NoticeRow,
// EventFrame (redeems / Bits), ModFeedRow, AutomodRow, rowHighlight and the Bits / channel-points icons.
// Changes: Chatterino-dense markup (RowFrame, 13px font, no per-row cards), settings come from the
// app's Settings (deletedMessages strike|hide|show, alternateRows, showBadges, readableColors,
// sevenTvCosmetics, timestamps), rows are memoized and read their pane through RowCtx / TwitchEnv /
// Handlers, inline mod buttons (modMode) replace the site's ModIconRow, the hover bar gained Delete.
import { memo, useState, type CSSProperties, type ReactNode } from "react";
import { WarningIcon } from "../../icons";
import { textMentionsLogin } from "../chatText";
import { Badges, MessageText } from "../parts";
import { modReach } from "../modCaps";
import { BADGE_PX, BADGE_VMARGIN } from "../twitch/badges";
import { nameColor } from "../twitch/colors";
import { paintStyle, use7TvStyle } from "../twitch/cosmetics";
import { BUILTIN_REDEEMS, DEFAULT_EVENTS, noticeKind } from "../twitch/events";
import { buildParts } from "../twitch/parts";
import type { ChatMsg, ChatUser, HeldMsg, ModFeedEntry, Notice, Row, SuspiciousUser } from "../types";
import type { RowCtx, TwitchEnv } from "./env";
import { HoverButtons, ModButtons, RowFrame } from "./RowFrame";

type MsgRowT = Extract<Row, { k: "msg" }>;
type NoticeRowT = Extract<Row, { k: "notice" }>;
type ModFeedRowT = Extract<Row, { k: "modfeed" }>;
type AutomodRowT = Extract<Row, { k: "automod" }>;

// ---------------------------------------------------------------------------
// Highlights: a tint on the whole row plus a small label. Priority: suspicious (restricted > monitored)
// > mentions you > first message > returning. The highlighted-message tint (redemption) never gets a label.
// ---------------------------------------------------------------------------

type Highlight = { style?: CSSProperties; label?: { text: string; className: string } };

function rowHighlight(msg: ChatMsg, mentionsMe: boolean, susp: SuspiciousUser["status"] | undefined): Highlight {
  if (susp === "restricted") {
    return { style: { borderLeft: "3px solid #ff5c1a", background: "rgba(255,92,26,.10)", paddingLeft: 6 }, label: { text: "RESTRICTED SUSPICIOUS USER", className: "text-orange-300" } };
  }
  if (susp === "active_monitoring") {
    return { style: { borderLeft: "3px solid #ff8c00", background: "rgba(255,140,0,.10)", paddingLeft: 6 }, label: { text: "MONITORED SUSPICIOUS USER", className: "text-orange-300" } };
  }
  if (mentionsMe) {
    return { style: { borderLeft: "3px solid #ff3b3b", background: "rgba(255,30,30,.12)", paddingLeft: 6 }, label: { text: "MENTIONS YOU", className: "text-red-300" } };
  }
  if (msg.firstMsg) {
    return { style: { borderLeft: "3px solid #9147ff", background: "rgba(145,71,255,.08)", paddingLeft: 6 }, label: { text: "FIRST MESSAGE", className: "text-purple-300" } };
  }
  if (msg.returning) {
    return { style: { borderLeft: "3px solid #2b8fef", background: "rgba(43,143,239,.08)", paddingLeft: 6 }, label: { text: "RETURNING CHATTER", className: "text-sky-300" } };
  }
  return {};
}

// ---------------------------------------------------------------------------
// Event frames: redeems, Power-ups, cheers
// ---------------------------------------------------------------------------

// Twitch's channel points glyph.
function ChannelPointsIcon({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 20 20" fill="currentColor" className={className} aria-label="channel points">
      <path d="M10 6a4 4 0 014 4h-2a2 2 0 00-2-2V6z" />
      <path fillRule="evenodd" clipRule="evenodd" d="M18 10a8 8 0 11-16 0 8 8 0 0116 0zm-2 0a6 6 0 11-12 0 6 6 0 0112 0z" />
    </svg>
  );
}

// Twitch's cheer gem changes shape and colour with the amount: grey at 1, purple at 100, teal at 1,000,
// blue at 5,000 and red at 10,000.
const BITS_TIERS: { min: number; color: string; points: string }[] = [
  { min: 10000, color: "#f43021", points: "10,1 12.6,7.2 19.5,7.6 14.2,12 15.9,18.7 10,15 4.1,18.7 5.8,12 0.5,7.6 7.4,7.2" },
  { min: 5000, color: "#0099fe", points: "10,1 18,5.5 18,14.5 10,19 2,14.5 2,5.5" },
  { min: 1000, color: "#1db2a5", points: "10,1 19,8 15.5,18.5 4.5,18.5 1,8" },
  { min: 100, color: "#9c3ee8", points: "10,1 17,8 10,19 3,8" },
  { min: 0, color: "#979797", points: "10,2 18,17 2,17" },
];

function BitsIcon({ amount, className }: { amount: number; className?: string }) {
  const tier = BITS_TIERS.find((t) => amount >= t.min) || BITS_TIERS[BITS_TIERS.length - 1];
  return (
    <svg viewBox="0 0 20 20" className={className} aria-label="bits">
      <polygon points={tier.points} fill={tier.color} />
      <polygon points={tier.points} fill="white" opacity="0.25" style={{ clipPath: "polygon(0 0, 100% 0, 100% 45%, 0 45%)" }} />
    </svg>
  );
}

// A redeem, Power-up or cheer: "<name> redeemed <reward>" with the price on the right, and the message
// (when there is one) underneath.
function EventFrame({
  user,
  color,
  verb,
  reward,
  bits,
  tone,
  children,
}: {
  user: ChatUser;
  color: string;
  verb: string;
  reward?: { title: string; cost?: number | null; bits?: number };
  bits?: number;
  tone: "redeem" | "cheer";
  children?: ReactNode;
}) {
  const amount = bits ?? reward?.bits ?? reward?.cost ?? null;
  const isBits = bits != null || reward?.bits != null || (tone === "cheer" && amount == null);
  return (
    <div className="my-px mx-1 rounded border border-purple-500/40 bg-purple-500/[0.07]">
      <div className={`flex items-center gap-2 px-2 pt-0.5 ${children ? "" : "pb-0.5"}`}>
        <span className="min-w-0 flex-1 truncate text-muted">
          <span className="font-semibold" style={{ color }}>
            {user.name}
          </span>{" "}
          {verb}
          {reward && (
            <>
              {" "}
              <span className="font-semibold text-text">{reward.title}</span>
            </>
          )}
        </span>
        <span className="flex shrink-0 items-center gap-1 font-semibold tabular-nums text-muted">
          {isBits ? <BitsIcon amount={amount ?? 1} className="h-[1em] w-[1em]" /> : amount != null && <ChannelPointsIcon className="h-[1em] w-[1em] text-accent-soft" />}
          {amount != null && amount.toLocaleString()}
        </span>
      </div>
      {children}
    </div>
  );
}

// ---------------------------------------------------------------------------
// A chat message
// ---------------------------------------------------------------------------

const EMPTY_KNOWN = new Map();

function MessageRow({ row, ctx, env, susp, modMode }: { row: MsgRowT; ctx: RowCtx; env: TwitchEnv; susp: SuspiciousUser["status"] | undefined; modMode: boolean }) {
  const { rs, h } = ctx;
  const msg = row.m;
  const cosmeticsOn = rs.sevenTvCosmetics;
  const style = use7TvStyle(msg.user.id, cosmeticsOn);
  const paint = cosmeticsOn && style?.paintId ? env.sevenDefs?.paints.get(style.paintId) : undefined;
  const sevenBadge = cosmeticsOn && style?.badgeId ? env.sevenDefs?.badges.get(style.badgeId) : undefined;

  if (msg.deleted && rs.deletedMessages === "hide") return null;
  const color = rs.readableColors ? nameColor(msg.user, true) : nameColor(msg.user, false);
  const nameStyle: CSSProperties = paint ? paintStyle(paint) : { color };
  const highlighted = msg.msgId === "highlighted-message";
  const mentionsMe = textMentionsLogin(msg.text, env.meLoginLower);
  const hl = rowHighlight(msg, mentionsMe, susp);
  const parts = buildParts(msg.text, msg.emotes, env.emotes, env.known ?? EMPTY_KNOWN, msg.gifs);
  const reach = modReach(env.viewer, { id: msg.user.id, badges: msg.badges });
  const caps = env.caps;
  const canBan = caps.ban && reach.ban;
  const canTimeout = caps.ban && reach.timeout;
  const canDelete = caps.delete && reach.delete && !msg.deleted;
  const struck = msg.deleted && rs.deletedMessages === "strike";

  // Command buttons need some mod right here; Twitch answers for the exact command.
  const isMod = caps.ban || caps.delete || caps.warn;
  const mod = modMode ? (
    <ModButtons
      buttons={ctx.modList}
      can={(b) =>
        b.action === "ban" ? canBan && !msg.deleted : b.action === "timeout" ? canTimeout && !msg.deleted : b.action === "delete" ? canDelete : isMod
      }
      onRun={(b, target) => {
        if (b.action === "ban") h.modPopover(target, "ban", row.sk, msg);
        else if (b.action === "timeout") h.quickTimeout(row.sk, msg, b.seconds);
        else if (b.action === "delete") h.deleteMsg(row.sk, msg.id);
        else h.twitchCommand(row.sk, msg, b.command);
      }}
    />
  ) : null;

  return (
    <RowFrame
      ctx={ctx}
      n={row.ord}
      ts={msg.ts}
      platform="twitch"
      className={highlighted && !hl.style ? "bg-purple-500/10" : ""}
      style={hl.style}
      label={hl.label}
      mod={mod}
      before={
        msg.reply ? (
          <div className="truncate text-faint" style={{ fontSize: "0.85em" }}>
            ↩ replying to @{msg.reply.user}: {msg.reply.text}
          </div>
        ) : undefined
      }
      hover={
        <HoverButtons
          onCopy={() => h.copy(msg.text)}
          onReply={() => h.reply({ sk: row.sk, platform: "twitch", id: msg.id, user: msg.user.name, text: msg.text })}
          onPin={caps.pin ? () => h.pin(row.sk, msg.id) : undefined}
          onDelete={canDelete ? () => h.deleteMsg(row.sk, msg.id) : undefined}
        />
      }
    >
      <button
        type="button"
        className="inline-flex items-baseline gap-1 align-baseline font-semibold hover:underline"
        onClick={(e) => h.openCard(e.currentTarget, row.sk, { id: msg.user.id, login: msg.user.login, name: msg.user.name })}
      >
        {rs.showBadges && <Badges tag={msg.badges} map={env.badgeMap} />}
        {rs.showBadges && sevenBadge && (
          <img
            src={`https://cdn.7tv.app/badge/${style!.badgeId}/3x`}
            title={sevenBadge.tooltip || sevenBadge.name}
            alt=""
            decoding="async"
            className="inline-block self-center"
            style={{ height: BADGE_PX, width: BADGE_PX, margin: `${BADGE_VMARGIN}px 0` }}
          />
        )}
        <span style={nameStyle}>{msg.user.name}</span>
      </button>
      {!msg.action && <span className="text-muted">:</span>}{" "}
      <span className={`${msg.action ? "font-medium" : "text-text"} ${struck ? "line-through opacity-50" : ""}`} style={msg.action ? nameStyle : undefined}>
        <MessageText
          parts={parts}
          size={rs.emoteSize}
          fontPx={rs.fontSize}
          emotes={env.emotes}
          onMentionClick={(e, who) => h.openCard(e.currentTarget, row.sk, { id: who.userId, login: who.login, name: who.name })}
        />
      </span>
    </RowFrame>
  );
}

/** A message row, wrapped in the redeem / cheer frame when it is one. */
export const TwitchMessageRow = memo(function TwitchMessageRow({
  row,
  ctx,
  env,
  susp,
  modMode,
}: {
  row: MsgRowT;
  ctx: RowCtx;
  env: TwitchEnv;
  susp: SuspiciousUser["status"] | undefined;
  modMode: boolean;
}) {
  const en = row.m;
  const inner = <MessageRow row={row} ctx={ctx} env={env} susp={susp} modMode={modMode} />;
  const ev = DEFAULT_EVENTS;
  // A Power-up comes in on custom-reward-id like a channel-point redeem; only the reward info says which
  // it was. Until that is known the header just says "redeemed" with no price.
  const builtin = en.msgId ? BUILTIN_REDEEMS[en.msgId] : undefined;
  const isBits = !!(en.reward?.bits || builtin?.bits);
  const frameColor = nameColor(en.user, ctx.rs.readableColors);
  if ((en.rewardId || builtin) && !en.deleted && (isBits ? ev.cheers : ev.redeems)) {
    return (
      <EventFrame user={en.user} color={frameColor} verb="redeemed" reward={builtin ? { title: builtin.title } : en.reward} tone={isBits ? "cheer" : "redeem"}>
        {inner}
      </EventFrame>
    );
  }
  if (en.bits && ev.cheers && !en.deleted) {
    return (
      <EventFrame user={en.user} color={frameColor} verb="cheered" bits={en.bits} tone="cheer">
        {inner}
      </EventFrame>
    );
  }
  return inner;
});

// ---------------------------------------------------------------------------
// Notices (sub / resub / gift / raid / announcement / redeem without text ...)
// ---------------------------------------------------------------------------

export const NoticeRow = memo(function NoticeRow({ row, ctx, env }: { row: NoticeRowT; ctx: RowCtx; env: TwitchEnv }) {
  const { rs } = ctx;
  const notice: Notice = row.n;
  const raid = noticeKind(notice) === "raids";
  if ((notice.msgId === "redeem" || notice.msgId === "powerup") && notice.reward) {
    // A redeem with no message: the same header as one with a message, just nothing under it.
    return (
      <EventFrame
        user={notice.user}
        color={nameColor(notice.user, rs.readableColors)}
        verb="redeemed"
        reward={notice.reward}
        tone={notice.reward.bits ? "cheer" : "redeem"}
      />
    );
  }
  // The user's own attached message goes through the full emote pipeline (Twitch positions from
  // notice.emotes, then 7TV/BTTV/FFZ name matching), as a separate line under the system text.
  const parts = notice.text ? buildParts(notice.text, notice.emotes || "", env.emotes) : [];
  return (
    <div
      className={`border-l-2 px-2 py-0.5 ${raid ? "border-orange-500/70 bg-orange-500/10" : "border-purple-500/70 bg-purple-500/5"}`}
      style={{ lineHeight: 1.45 }}
    >
      <div className={`font-semibold ${raid ? "text-orange-200" : "text-purple-200"}`}>{notice.system}</div>
      {notice.text && (
        <div className="text-muted">
          <MessageText parts={parts} size={rs.emoteSize} fontPx={rs.fontSize} emotes={env.emotes} />
        </div>
      )}
    </div>
  );
});

// ---------------------------------------------------------------------------
// Anonymous mod-action / room-mode feed lines: dim slate system rows, no interactions.
// ---------------------------------------------------------------------------

export const ModFeedRow = memo(function ModFeedRow({ row, ctx }: { row: ModFeedRowT; ctx: RowCtx }) {
  const entry: ModFeedEntry = row.f;
  return (
    <div className="flex items-baseline gap-2 px-2 py-px italic text-slate-500" style={{ fontSize: "0.9em", lineHeight: 1.45 }}>
      {ctx.rs.timestamps && <span className="shrink-0 text-slate-600">{new Date(entry.ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</span>}
      <span className="min-w-0 flex-1 truncate">{entry.text}</span>
    </div>
  );
});

// ---------------------------------------------------------------------------
// AutoMod held-message row: amber-tinted, like Twitch's own mod view. Allow/Deny while held (caps.automod
// gated); once resolved the buttons become a dim "Allowed/Denied/Expired by ..." line (driven by the
// authoritative "automodupdate" broadcast, not local optimism).
// ---------------------------------------------------------------------------

export const AutomodRow = memo(function AutomodRow({ row, ctx, env }: { row: AutomodRowT; ctx: RowCtx; env: TwitchEnv }) {
  const [busy, setBusy] = useState(false);
  const entry: HeldMsg = row.h;
  const chip =
    entry.reason === "blocked_term" ? "Blocked term" : entry.category ? `${entry.category}${entry.level != null ? ` (level ${entry.level})` : ""}` : "AutoMod";
  const act = async (action: "allow" | "deny") => {
    setBusy(true);
    try {
      await ctx.h.automod(row.sk, entry.id, action);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="my-px rounded px-2 py-1" style={{ borderLeft: "3px solid #f59e0b", background: "rgba(245,158,11,.08)", lineHeight: 1.45 }}>
      <div className="flex flex-wrap items-center gap-1.5 text-amber-200" style={{ fontSize: "0.92em" }}>
        <WarningIcon className="h-3.5 w-3.5 shrink-0" />
        <span>
          AutoMod held a message from{" "}
          <button
            type="button"
            className="font-semibold hover:underline"
            onClick={(e) => ctx.h.openCard(e.currentTarget, row.sk, { id: entry.user.id, login: entry.user.login, name: entry.user.name })}
          >
            {entry.user.name}
          </button>
        </span>
        <span className="rounded-full border border-amber-500/30 bg-amber-500/10 px-1.5 py-px text-[10px] text-amber-300">{chip}</span>
      </div>
      <div className="whitespace-normal break-words text-text">{entry.text}</div>
      {entry.status === "held" ? (
        env.caps.automod && (
          <div className="mt-1 flex gap-1.5">
            <button
              type="button"
              disabled={busy}
              onClick={() => act("allow")}
              className="rounded border border-green-500/30 bg-green-500/10 px-2 py-px text-[11px] font-medium text-green-300 hover:border-green-500/50 disabled:opacity-40"
            >
              Allow
            </button>
            <button
              type="button"
              disabled={busy}
              onClick={() => act("deny")}
              className="rounded border border-red-500/30 bg-red-500/10 px-2 py-px text-[11px] font-medium text-red-300 hover:border-red-500/50 disabled:opacity-40"
            >
              Deny
            </button>
          </div>
        )
      ) : (
        <div className="text-[11px] text-faint">
          {entry.status === "approved" ? `Allowed by ${entry.resolvedBy || "?"}` : entry.status === "denied" ? `Denied by ${entry.resolvedBy || "?"}` : "Expired (no action taken)"}
        </div>
      )}
    </div>
  );
});

