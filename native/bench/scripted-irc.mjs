// Scripted fake Twitch IRC server: every client that joins a channel gets the SAME fixed set of lines (badges,
// emotes, a first message, a reply, a deleted message, cheers, a redeem, /me, a USERNOTICE of each kind, a
// timeout, a very long word, filler), so two chat clients can be compared screenshot against screenshot.
//   node native/bench/scripted-irc.mjs        -> ws://127.0.0.1:6681   (PORT=..., FILLER=34 trailing filler lines)
// Point a client at it with AGU_IRC_URL=ws://127.0.0.1:6681. Needs the `ws` package (the repo's node_modules).
import { WebSocketServer } from "ws";

const PORT = Number(process.env.PORT || 6681);
const FILLER = Number(process.env.FILLER ?? 34); // trailing filler lines (0 = just the interesting ones)
const ROOM_ID = "156200816"; // a_g_u__, so the real 7TV / BTTV / FFZ channel emotes resolve
const T0 = Date.UTC(2026, 10, 5, 19, 41, 7); // fixed so both apps print the same clock

async function getJson(url) {
  try {
    const r = await fetch(url, { headers: { "User-Agent": "AguChat-native-prototype/0.1 (bench)" } });
    return r.ok ? await r.json() : null;
  } catch {
    return null;
  }
}
const g7 = await getJson("https://7tv.io/v3/emote-sets/global");
const c7 = await getJson(`https://7tv.io/v3/users/twitch/${ROOM_ID}`);
const gb = await getJson("https://api.betterttv.net/3/cached/emotes/global");
const names7 = (g7?.emotes || []).filter((e) => !(e.flags & 1) && !(e.data?.flags & 256) && !/\s/.test(e.name)).map((e) => e.name);
const chan7 = (c7?.emote_set?.emotes || []).filter((e) => !(e.flags & 1) && !(e.data?.flags & 256)).map((e) => e.name);
const namesB = (Array.isArray(gb) ? gb : []).filter((e) => !e.modifier).map((e) => e.code);
const E = (list, i, fallback) => list[i] ?? fallback;
const e1 = E(names7, 0, "KEKW"), e2 = E(names7, 3, "Sadge"), e3 = E(namesB, 1, "OMEGALUL"), e4 = E(chan7, 0, e1);

function esc(s) {
  return s.replace(/\\/g, "\\\\").replace(/;/g, "\\:").replace(/ /g, "\\s").replace(/\r/g, "\\r").replace(/\n/g, "\\n");
}
const U = {
  agu: { login: "a_g_u__", name: "a_g_u__", id: "156200816", color: "#9147FF", badges: "broadcaster/1,subscriber/0" },
  cool: { login: "cooldude", name: "CoolDude", id: "20002", color: "#FF4500", badges: "moderator/1,subscriber/12" },
  dim: { login: "dimguy", name: "dimguy", id: "20003", color: "#1A1A8A", badges: "" },
  mod: { login: "modperson", name: "ModPerson", id: "20004", color: "#00FF7F", badges: "moderator/1,vip/1" },
  vip: { login: "vipvera", name: "VipVera", id: "20005", color: "#FF69B4", badges: "vip/1,subscriber/3" },
  plain: { login: "plainjoe", name: "PlainJoe", id: "20006", color: "", badges: "" },
  gifter: { login: "generoso", name: "Generoso", id: "20007", color: "#DAA520", badges: "subscriber/24" },
};
let n = 0;
const id = () => `00000000-0000-4000-a000-${String(++n).padStart(12, "0")}`;
let ts = T0;
const tick = () => (ts += 3000 + (n % 4) * 700);

function priv(ch, u, text, extra = {}) {
  const msgId = extra.id || id();
  const tags = {
    badges: u.badges, color: u.color, "display-name": esc(u.name), emotes: extra.emotes || "", "first-msg": extra.first ? "1" : "0", id: msgId,
    mod: u.badges.includes("moderator") ? "1" : "0", "room-id": ROOM_ID, subscriber: u.badges.includes("subscriber") ? "1" : "0",
    "tmi-sent-ts": String(tick()), "user-id": u.id, ...(extra.tags || {}),
  };
  const t = Object.entries(tags).map(([k, v]) => `${k}=${v}`).join(";");
  const body = extra.action ? `\u0001ACTION ${text}\u0001` : text;
  return { line: `@${t} :${u.login}!${u.login}@${u.login}.tmi.twitch.tv PRIVMSG #${ch} :${body}`, id: msgId };
}
function usernotice(ch, u, kind, sys, text = "", extra = {}) {
  const tags = {
    badges: u.badges, color: u.color, "display-name": esc(u.name), emotes: "", id: id(), login: u.login, "msg-id": kind, "room-id": ROOM_ID,
    "system-msg": esc(sys), "tmi-sent-ts": String(tick()), "user-id": u.id, ...extra,
  };
  const t = Object.entries(tags).map(([k, v]) => `${k}=${v}`).join(";");
  return `@${t} :tmi.twitch.tv USERNOTICE #${ch}${text ? ` :${text}` : ""}`;
}

function script(ch) {
  const out = [];
  const add = (m) => out.push(m.line ?? m);
  add(priv(ch, U.agu, "hello chat Kappa " + e1 + " " + e3, { emotes: "25:11-15" }));
  add(priv(ch, U.cool, `this is a fairly long message that is going to wrap onto a second line so we can see how the dense layout and the line height behave when text wraps ${e2} in a narrow split ${e4} and ${e1} ${e1}`));
  add(priv(ch, U.dim, "first time here! LUL", { first: true, emotes: "425618:18-20" }));
  add(priv(ch, U.mod, "@a_g_u__ you are mentioned, check https://example.com/page. and twitch.tv/a_g_u__", {}));
  const parent = priv(ch, U.agu, "what are we playing today?");
  add(parent);
  add(priv(ch, U.cool, "@a_g_u__ replying to you with something", { tags: { "reply-parent-msg-id": parent.id, "reply-parent-user-id": U.agu.id, "reply-parent-user-login": U.agu.login, "reply-parent-display-name": U.agu.name, "reply-parent-msg-body": esc("what are we playing today?") } }));
  const doomed = priv(ch, U.plain, "this one will be deleted");
  add(doomed);
  add(priv(ch, U.vip, "cheer100 take my bits", { tags: { bits: "100" } }));
  add(priv(ch, U.cool, "redeem text goes here", { tags: { "custom-reward-id": "00000000-aaaa-bbbb-cccc-000000000001" } }));
  add(priv(ch, U.mod, "this message is highlighted", { tags: { "msg-id": "highlighted-message" } }));
  add(priv(ch, U.vip, "waves at everyone", { action: true }));
  add(usernotice(ch, U.cool, "resub", `${U.cool.name} subscribed at Tier 1. They've subscribed for 12 months!`, "love the stream " + e1, { "msg-param-cumulative-months": "12", "msg-param-sub-plan": "1000" }));
  add(usernotice(ch, U.gifter, "subgift", `${U.gifter.name} gifted a Tier 1 sub to ${U.plain.name}!`, "", { "msg-param-recipient-display-name": U.plain.name, "msg-param-recipient-user-name": U.plain.login, "msg-param-sub-plan": "1000" }));
  add(usernotice(ch, U.dim, "raid", "5 raiders from SomeChannel have joined!", "", { "msg-param-viewerCount": "5", "msg-param-displayName": "SomeChannel" }));
  add(priv(ch, U.plain, "W".repeat(90)));
  for (let i = 0; i < 6; i++) add(priv(ch, [U.cool, U.dim, U.plain, U.vip][i % 4], `filler message number ${i + 1} ${i % 2 ? e1 : "gg"}`));
  add(`@room-id=${ROOM_ID};target-msg-id=${doomed.id};login=${U.plain.login};tmi-sent-ts=${tick()} :tmi.twitch.tv CLEARMSG #${ch} :this one will be deleted`);
  add(`@ban-duration=600;room-id=${ROOM_ID};target-user-id=${U.dim.id};tmi-sent-ts=${tick()} :tmi.twitch.tv CLEARCHAT #${ch} :${U.dim.login}`);
  for (let i = 6; i < 6 + FILLER; i++) add(priv(ch, [U.cool, U.dim, U.plain, U.vip, U.mod][i % 5], `filler message number ${i + 1}`));
  return out;
}

const wss = new WebSocketServer({ host: "127.0.0.1", port: PORT });
console.log(`scripted IRC listening on ws://127.0.0.1:${PORT} (emotes: ${e1} ${e2} ${e3} ${e4})`);
wss.on("connection", (ws) => {
  let nick = "justinfan00000";
  const send = (l) => ws.readyState === 1 && ws.send(l);
  ws.on("message", (data) => {
    for (const line of String(data).split(/\r?\n/)) {
      if (!line) continue;
      const [cmd, ...rest] = line.split(" ");
      if (cmd === "NICK") {
        nick = rest[0] || nick;
        for (const l of [`001 ${nick} :Welcome, GLHF!`, `002 ${nick} :Your host is tmi.twitch.tv`, `003 ${nick} :-`, `004 ${nick} :-`, `375 ${nick} :-`, `372 ${nick} :-`, `376 ${nick} :>`]) send(`:tmi.twitch.tv ${l}`);
      } else if (cmd === "CAP") send(`:tmi.twitch.tv CAP * ACK :${line.split(":").slice(1).join(":") || "twitch.tv/tags twitch.tv/commands"}`);
      else if (cmd === "JOIN") {
        for (const c of (rest[0] || "").split(",")) {
          const ch = c.replace(/^#/, "").toLowerCase();
          if (!ch) continue;
          send(`:${nick}!${nick}@${nick}.tmi.twitch.tv JOIN #${ch}`);
          send(`@emote-only=0;followers-only=-1;r9k=0;room-id=${ROOM_ID};slow=0;subs-only=0 :tmi.twitch.tv ROOMSTATE #${ch}`);
          n = 0;
          ts = T0;
          const lines = script(ch);
          setTimeout(() => lines.forEach((l) => send(l)), 1500);
        }
      } else if (cmd === "PING") send(`:tmi.twitch.tv PONG tmi.twitch.tv :${rest.join(" ").replace(/^:/, "")}`);
    }
  });
});
