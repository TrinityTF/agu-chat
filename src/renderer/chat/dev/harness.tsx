// Dev-only harness for the chat view: open http://localhost:5199/chat/dev/index.html in the browser pane.
// Installs a fake window.agu (frames shaped like the real relay's, replayed BEFORE subscribe() resolves like
// main does) and renders one or two ChatPane splits. Not part of the production bundle (not imported by
// index.html).
import type { AguBridge, Frame, Settings, SourceRef } from "../../protocol";

const now = Date.now();
const user = (id: string, login: string, color = "") => ({ id, login, name: login[0].toUpperCase() + login.slice(1), color });
const msg = (i: number, u: ReturnType<typeof user>, text: string, extra: Record<string, unknown> = {}) => ({
  platform: "twitch",
  id: "m" + i,
  ts: now - (60 - i) * 4000,
  user: u,
  badges: "",
  text,
  emotes: "",
  action: false,
  firstMsg: false,
  reply: null,
  rewardId: null,
  msgId: null,
  ...extra,
});

const SVG = (c: string) => "data:image/svg+xml;utf8," + encodeURIComponent(`<svg xmlns='http://www.w3.org/2000/svg' width='36' height='36'><rect width='36' height='36' rx='6' fill='${c}'/></svg>`);

const a = user("1", "a_g_u__", "#9147ff");
const b = user("2", "cooldude", "#ff4500");
const c = user("3", "dimguy", "#1a1a8a");
const mod = user("4", "modperson", "#00ff7f");

function twitchLog(): Frame {
  const messages: any[] = [
    msg(1, a, "hello chat Kappa 7TV test", { badges: "broadcaster/1", emotes: "25:11-15" }),
    msg(2, b, "this is a fairly long message that is going to wrap onto a second line so we can see how the dense layout and the line height behave when text wraps in a narrow split", { badges: "subscriber/12,moderator/1" }),
    msg(3, c, "first time here!", { firstMsg: true }),
    msg(4, mod, "@a_g_u__ you are mentioned, check https://example.com/page.", { badges: "moderator/1" }),
    msg(5, b, "replying to you", { reply: { user: "a_g_u__", text: "hello chat Kappa 7TV test" } }),
    msg(6, c, "this one will be deleted", { deleted: true }),
    msg(7, b, "cheer100 take my bits", { bits: 100 }),
    msg(8, c, "redeem text", { rewardId: "r1", reward: { title: "Hydrate", cost: 500 } }),
    msg(9, a, "waves", { action: true }),
    { notice: true, id: "n1", ts: now - 30000, system: "cooldude subscribed with Prime. They've subscribed for 12 months!", msgId: "resub", user: b, text: "love the stream", emotes: "" },
    { notice: true, id: "n2", ts: now - 29000, system: "5 raiders from somechannel have joined!", msgId: "raid", user: c, text: "" },
  ];
  for (let i = 10; i < 40; i++) messages.push(msg(i, i % 2 ? b : c, "filler message number " + i));
  return {
    type: "chatlog",
    channelId: "12826",
    messages,
    pinned: { ...msg(100, mod, "Pinned: be nice to each other", { badges: "moderator/1", pin: { by: { id: "4", login: "modperson", name: "Modperson" }, startsAt: null, endsAt: null } }) },
    modfeed: [{ ts: now - 20000, kind: "timeout", text: "A moderator timed out someone for 10m" }],
    automod: [{ id: "h1", ts: now - 15000, user: { id: "9", login: "sketchy", name: "Sketchy" }, text: "some borderline message", reason: "automod", category: "swearing", level: 2, status: "held", resolvedBy: null }],
    suspicious: [{ id: "3", login: "dimguy", name: "Dimguy", status: "active_monitoring", types: ["manually_added"], evaluation: null, sharedBanChannels: 0 }],
    esub: { active: true, reason: null },
  };
}

const rumbleLog: Frame = {
  type: "chatlog",
  messages: [
    { platform: "rumble", id: "r1", ts: now - 25000, user: { id: "ru1", login: "rumbler", name: "Rumbler", color: "#85c742", avatar: "" }, badges: [], text: "hey from rumble :r+cooldoge: yo", emotes: "", action: false, firstMsg: false, reply: null, isFollower: true, channel: null, rant: null, notice: null },
    { platform: "rumble", id: "r2", ts: now - 18000, user: { id: "ru2", login: "rich", name: "Rich", color: "#ffaa00", avatar: "" }, badges: [], text: "have a rant", emotes: "", action: false, firstMsg: false, reply: null, isFollower: true, channel: null, rant: { cents: 500, duration: 60, expiresAt: now + 60000 }, notice: null },
  ],
  status: { live: true },
  emotes: { "r+cooldoge": { file: SVG("#85c742") } },
  badges: {},
};
const kickLog: Frame = {
  type: "chatlog",
  messages: [
    { platform: "kick", id: "k1", ts: now - 22000, user: { id: "ki1", login: "kicker", name: "Kicker", color: "#53fc18", avatar: "", verified: true, anonymous: false }, badges: [{ type: "moderator", text: "Moderator", count: 0 }, { type: "subscriber", text: "Subscriber", count: 3 }], text: "kick message [emote:37226:KEKW] nice", reply: null, action: false, notice: null },
  ],
  status: { live: true },
  emotes: { KEKW: { id: "37226", group: "global" } },
};
const ytLog: Frame = {
  type: "chatlog",
  messages: [
    { platform: "youtube", id: "y1", ts: now - 21000, user: { id: "yu1", name: "YTViewer", avatar: "" }, badges: [{ label: "Member", icon: "", url: "" }], parts: [{ t: "text", v: "hello youtube " }], text: "hello youtube", paid: null, notice: null },
    { platform: "youtube", id: "y2", ts: now - 14000, user: { id: "yu2", name: "BigSpender", avatar: "" }, badges: [], parts: [{ t: "text", v: "super chat!" }], text: "super chat!", paid: { amount: "$10.00", bg: "rgba(230,81,0,.5)", headerBg: "", sticker: false }, notice: null },
  ],
  status: { live: true },
};
const xLog: Frame = {
  type: "chatlog",
  messages: [{ platform: "x", id: "x1", ts: now - 19000, user: { id: "xu1", name: "XPerson", login: "xperson", avatar: "", twitterId: "1" }, text: "hello from X https://x.com" }],
  status: { live: true },
};

const settings: Settings = {
  twitchClientId: "",
  updateChannel: "beta",
  timestamps: true,
  timestampSeconds: false,
  alternateRows: true,
  fontSize: 13,
  emoteSize: "medium",
  showBadges: true,
  readableColors: true,
  sevenTv: true,
  bttv: true,
  ffz: true,
  sevenTvCosmetics: true,
  deletedMessages: "strike",
  showModButtons: true,
  showPlatformMarks: true,
  highlightRules: [],
  mentionSound: false,
  mentionFlash: true,
  highlightSoundFile: "",
  highlightVolume: 70,
  highlightSoundFocused: true,
};

const frameCbs = new Set<(id: string, f: Frame) => void>();
const settingsCbs = new Set<(s: Settings) => void>();
const subs = new Map<string, SourceRef>();
let nextSub = 1;
const apiLog: unknown[] = [];
let canSend = true;

const emit = (id: string, f: Frame) => frameCbs.forEach((cb) => cb(id, f));

const agu: AguBridge = {
  async subscribe(source) {
    const id = "sub" + nextSub++;
    subs.set(id, source);
    // Like main/relay.js: the snapshot is sent BEFORE the invoke answers.
    const conn: Frame = { type: "conn", state: "open", live: true, canSend: source.platform === "x" || source.platform === "youtube" ? false : canSend, readOnlyReason: source.platform === "youtube" ? "YouTube is read-only in this beta" : source.platform === "x" ? "X is read-only" : canSend ? undefined : "Log in to Twitch to chat", channelId: undefined } as any;
    emit(id, conn);
    const log = { twitch: twitchLog(), rumble: rumbleLog, kick: kickLog, youtube: ytLog, x: xLog }[source.platform];
    emit(id, log);
    await new Promise((r) => setTimeout(r, 30));
    return id;
  },
  async unsubscribe(id) {
    subs.delete(id);
  },
  onFrame(cb) {
    frameCbs.add(cb);
    return () => frameCbs.delete(cb);
  },
  async api(source, path, init) {
    apiLog.push({ platform: source.platform, path, init });
    const p = path.split("?")[0];
    if (p === "/api/chat/badges")
      return { status: 200, body: { channelId: "12826", global: [{ set_id: "moderator", versions: [{ id: "1", image_url_2x: SVG("#00ad03"), title: "Moderator" }] }, { set_id: "broadcaster", versions: [{ id: "1", image_url_2x: SVG("#e91916"), title: "Broadcaster" }] }, { set_id: "subscriber", versions: [{ id: "12", image_url_2x: SVG("#6441a5"), title: "Sub 12" }] }], channel: [] } };
    if (p === "/api/chat/twitch-emotes") return { status: 200, body: { channel: [{ id: "1", name: "chanEmoteA", url: SVG("#f90") }], global: [{ id: "25", name: "Kappa", url: "https://static-cdn.jtvnw.net/emoticons/v2/25/default/dark/1.0" }], other: [] } };
    if (p === "/api/mod/caps") return { status: 200, body: { pin: true, ban: true, warn: true, delete: true, send: true, modes: true, announce: true, shoutout: true, shield: true, terms: true, raid: true, automod: true, loggedIn: true, isBroadcaster: true, isMod: true } };
    if (p === "/api/mod/user") return { status: 200, body: { user: { id: "2", login: "cooldude", displayName: "Cooldude", avatar: SVG("#888"), createdAt: "2020-01-02T00:00:00Z" }, followedAt: "2021-03-04T00:00:00Z", ban: null } };
    if (p === "/api/rumble/caps" || p === "/api/kick/caps") return { status: 200, body: { connected: true, canModerate: true, canDelete: true } };
    if (p === "/api/chat/send") {
      const body = typeof init?.body === "string" ? JSON.parse(init.body) : init?.body;
      for (const [id, s] of subs) if (s.platform === "twitch") emit(id, { type: "chat", msg: msg(Date.now() % 1e6, a, String((body as any).text), { ts: Date.now() }) });
      return { status: 200, body: { ok: true } };
    }
    return { status: 200, body: { ok: true } };
  },
  accounts: {
    get: async () => ({ twitch: { connected: true, user: { id: "1", login: "a_g_u__", name: "a_g_u__" } }, kick: { connected: false }, youtube: { connected: false }, rumble: { connected: true }, x: { connected: false } }) as any,
    login: async () => ({}),
    cancel: async () => {},
    logout: async () => {},
    onChange: () => () => {},
  },
  settings: {
    get: async () => ({ ...settings }),
    set: async (p) => {
      Object.assign(settings, p);
      settingsCbs.forEach((cb) => cb({ ...settings }));
      return { ...settings };
    },
    onChange: (cb) => {
      settingsCbs.add(cb);
      return () => settingsCbs.delete(cb);
    },
  },
  layout: { get: async () => null, set: async () => {} },
  updater: { get: async () => ({}) as any, check: async () => {}, install: async () => {}, onChange: () => () => {} },
  app: { info: async () => ({ version: "dev", packaged: false }), openExternal: async () => {}, flash: async () => {} },
  sounds: { pick: async () => null, read: async () => new Uint8Array() },
};

(window as any).agu = agu;
(window as any).__harness = { emit, subs, settings, apiLog, setCanSend: (v: boolean) => (canSend = v), setSettings: (p: Partial<Settings>) => agu.settings.set(p) };

async function main() {
  const [{ createRoot }, { StrictMode }, { ChatPane }] = await Promise.all([import("react-dom/client"), import("react"), import("../ChatPane")]);
  await import("../../index.css");
  const params = new URLSearchParams(location.search);
  const merged = params.get("mode") === "merged";
  const mod = params.get("mod") === "1";
  if (params.get("cansend") === "0") canSend = false;
  const root = document.getElementById("root")!;
  root.style.cssText = "height:100vh;display:flex;gap:1px;background:#26262f";
  const panes = merged
    ? [[{ platform: "twitch", channel: "a_g_u__" }, { platform: "rumble", channel: "rumbler" }, { platform: "kick", channel: "kicker" }, { platform: "youtube", channel: "@yt" }, { platform: "x", channel: "xperson" }]]
    : [[{ platform: "twitch", channel: "a_g_u__" }]];
  createRoot(root).render(
    <StrictMode>
      {panes.map((sources, i) => (
        <div key={i} style={{ flex: 1, minWidth: 0 }}>
          <ChatPane sources={sources as SourceRef[]} modMode={mod} focused={i === 0} onStatus={(s) => ((window as any).__status = s)} />
        </div>
      ))}
    </StrictMode>,
  );
}
void main();
