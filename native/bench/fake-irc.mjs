// Fake Twitch IRC server for benchmarking chat clients under identical, repeatable load.
//   node native/bench/fake-irc.mjs            -> ws://127.0.0.1:6680
//   RATE=20 SEED=1 PORT=6680 node ...         -> messages/sec per joined channel, RNG seed, port
// Point a client at it with AGU_IRC_URL=ws://127.0.0.1:6680 (no other streamer's chat is ever touched).
import { WebSocketServer } from "ws";

const PORT = Number(process.env.PORT || 6680);
const RATE = Number(process.env.RATE || 20);
const SEED = Number(process.env.SEED || 1);
// Real Twitch user id of a_g_u__ (looked up once via https://api.ivr.fi/v2/twitch/user?login=a_g_u__),
// used as the room-id for every fake channel so real 7TV/BTTV/FFZ channel emotes resolve.
const ROOM_ID = "156200816";

function mulberry32(a) {
  return function () {
    a |= 0; a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
const rnd = mulberry32(SEED);
const pick = (arr) => arr[Math.floor(rnd() * arr.length)];
const chance = (p) => rnd() < p;

async function getJson(url) {
  try {
    const r = await fetch(url, { headers: { "User-Agent": "AguChat-native-prototype/0.1 (bench)" } });
    if (!r.ok) return null;
    return await r.json();
  } catch {
    return null;
  }
}

async function loadEmoteNames() {
  const names = new Set();
  const sets = await Promise.all([
    getJson("https://7tv.io/v3/emote-sets/global"),
    getJson(`https://7tv.io/v3/users/twitch/${ROOM_ID}`),
    getJson("https://api.betterttv.net/3/cached/emotes/global"),
    getJson(`https://api.betterttv.net/3/cached/users/twitch/${ROOM_ID}`),
    getJson("https://api.betterttv.net/3/cached/frankerfacez/emotes/global"),
    getJson(`https://api.betterttv.net/3/cached/frankerfacez/users/twitch/${ROOM_ID}`),
  ]);
  const [g7, c7, gb, cb, gf, cf] = sets;
  for (const e of g7?.emotes || []) names.add(e.name);
  for (const e of c7?.emote_set?.emotes || []) names.add(e.name);
  for (const e of Array.isArray(gb) ? gb : []) names.add(e.code);
  for (const e of [...(cb?.channelEmotes || []), ...(cb?.sharedEmotes || [])]) names.add(e.code);
  for (const e of Array.isArray(gf) ? gf : []) names.add(e.code);
  for (const e of Array.isArray(cf) ? cf : []) names.add(e.code);
  const list = [...names].filter((n) => n && !/\s/.test(n));
  if (list.length < 10) list.push("KEKW", "OMEGALUL", "Sadge", "monkaS", "PogU", "catJAM", "peepoHappy", "FeelsStrongMan");
  return list;
}

const NATIVE = [
  ["Kappa", 25], ["LUL", 425618], ["4Head", 354], ["Kreygasm", 41], ["BibleThump", 86], ["PogChamp", 305954156],
];
const WORDS = (
  "yo what is this chat going lets gooo that was insane no way bro clip it actually crazy bruh lol lmao true same " +
  "first time here hello everyone how are you doing today nice play gg wp ez noob trash good game he cooked w l take " +
  "the stream is fire song name pls banger hype cant believe it wait what happened chat explain im dead dying laughing " +
  "back again again lets run it up sheesh based cringe fr fr no cap deadass huh wdym"
).split(" ");
const SPECIAL = ["\u65e5\u672c\u8a9e\u306e\u30e1\u30c3\u30bb\u30fc\u30b8", "\ud55c\uad6d\uc5b4 \uba54\uc2dc\uc9c0", "\ud83d\ude02\ud83d\udd25", "\u2764\ufe0f\u2728", "\u00bf\u00f3\u00e9\u00fc?", "\ud83d\udc80"];
const BADGES = [
  ["broadcaster/1", 0.002], ["moderator/1", 0.04], ["vip/1", 0.03], ["subscriber/0", 0.28], ["subscriber/3", 0.12],
];
const COLORS = ["#FF0000", "#0000FF", "#008000", "#B22222", "#FF7F50", "#9ACD32", "#FF4500", "#2E8B57", "#DAA520", "#D2691E", "#5F9EA0", "#1E90FF", "#FF69B4", "#8A2BE2", "#00FF7F", "#A970FF", ""];

let emoteNames = [];
const users = [];
function makeUsers() {
  for (let i = 0; i < 500; i++) {
    const login = `${pick(["xx", "the", "mr", "not", "big", "lil", "", "", ""])}${pick(["cat", "dog", "pog", "gamer", "wolf", "tv", "ninja", "toast", "frog", "mango", "salt", "void", "owl"])}${Math.floor(rnd() * 9999)}`;
    const display = chance(0.6) ? login[0].toUpperCase() + login.slice(1) : login;
    const badges = BADGES.filter(([, p]) => chance(p)).map(([b]) => b);
    users.push({ login, display, color: pick(COLORS), id: String(10000000 + i * 37), badges: [...new Set(badges)].join(",") });
  }
}

function escTag(s) {
  return s.replace(/\\/g, "\\\\").replace(/;/g, "\\:").replace(/ /g, "\\s").replace(/\r/g, "\\r").replace(/\n/g, "\\n");
}

let msgCounter = 0;
function newId() {
  msgCounter++;
  const h = (n) => Math.floor(rnd() * 16 ** n).toString(16).padStart(n, "0");
  return `${h(8)}-${h(4)}-4${h(3)}-a${h(3)}-${msgCounter.toString(16).padStart(12, "0")}`;
}

/** Build message text and the matching `emotes` tag (code-point positions). */
function makeText() {
  const n = 1 + Math.floor(rnd() * rnd() * 20);
  const parts = []; // {t, native?}
  for (let i = 0; i < n; i++) parts.push({ t: pick(WORDS) });
  if (chance(0.4)) {
    const k = 1 + Math.floor(rnd() * 3);
    for (let i = 0; i < k; i++) parts.splice(Math.floor(rnd() * (parts.length + 1)), 0, { t: pick(emoteNames) });
  }
  if (chance(0.09)) {
    const [name, id] = pick(NATIVE);
    parts.splice(Math.floor(rnd() * (parts.length + 1)), 0, { t: name, native: id });
  }
  if (chance(0.04)) parts.splice(Math.floor(rnd() * (parts.length + 1)), 0, { t: pick(SPECIAL) });
  if (chance(0.01)) parts.push({ t: "W".repeat(60 + Math.floor(rnd() * 60)) });
  let text = "";
  const emotes = new Map();
  for (const p of parts) {
    if (text) text += " ";
    const start = [...text].length;
    text += p.t;
    if (p.native) {
      const end = start + [...p.t].length - 1;
      if (!emotes.has(p.native)) emotes.set(p.native, []);
      emotes.get(p.native).push(`${start}-${end}`);
    }
  }
  const tag = [...emotes].map(([id, r]) => `${id}:${r.join(",")}`).join("/");
  return { text, tag };
}

const recent = new Map(); // channel -> [{id, user, text}]

function privmsg(ch) {
  const u = pick(users);
  const { text, tag } = makeText();
  const id = newId();
  const action = chance(0.03);
  const body = action ? `\u0001ACTION ${text}\u0001` : text;
  const list = recent.get(ch) || [];
  list.push({ id, user: u, text });
  if (list.length > 60) list.shift();
  recent.set(ch, list);
  const mod = u.badges.includes("moderator") ? 1 : 0;
  const sub = u.badges.includes("subscriber") ? 1 : 0;
  const tags = [
    `badge-info=${sub ? "subscriber/" + Math.floor(rnd() * 24) : ""}`, `badges=${u.badges}`, `color=${u.color}`,
    `display-name=${escTag(u.display)}`, `emotes=${tag}`, "first-msg=0", `id=${id}`, `mod=${mod}`, `room-id=${ROOM_ID}`,
    `subscriber=${sub}`, `tmi-sent-ts=${Date.now()}`, "turbo=0", `user-id=${u.id}`, "user-type=",
  ].join(";");
  return `@${tags} :${u.login}!${u.login}@${u.login}.tmi.twitch.tv PRIVMSG #${ch} :${body}`;
}

function usernotice(ch) {
  const u = pick(users);
  const kind = pick(["sub", "resub", "subgift", "raid"]);
  let sys;
  if (kind === "sub") sys = `${u.display} subscribed with Prime.`;
  else if (kind === "resub") sys = `${u.display} subscribed at Tier 1. They've subscribed for ${1 + Math.floor(rnd() * 30)} months!`;
  else if (kind === "subgift") sys = `${u.display} gifted a Tier 1 sub to ${pick(users).display}!`;
  else sys = `${1 + Math.floor(rnd() * 400)} raiders from ${u.display} have joined!`;
  const tags = [
    `badges=${u.badges}`, `color=${u.color}`, `display-name=${escTag(u.display)}`, "emotes=", `id=${newId()}`, `login=${u.login}`,
    `msg-id=${kind}`, `room-id=${ROOM_ID}`, `system-msg=${escTag(sys)}`, `tmi-sent-ts=${Date.now()}`, `user-id=${u.id}`,
  ].join(";");
  const msg = kind === "resub" && chance(0.6) ? ` :${makeText().text}` : "";
  return `@${tags} :tmi.twitch.tv USERNOTICE #${ch}${msg}`;
}

function clearmsg(ch) {
  const list = recent.get(ch);
  if (!list || !list.length) return null;
  const t = pick(list.slice(-30));
  return `@login=${t.user.login};room-id=${ROOM_ID};target-msg-id=${t.id};tmi-sent-ts=${Date.now()} :tmi.twitch.tv CLEARMSG #${ch} :${t.text}`;
}

function clearchat(ch) {
  const u = pick(users);
  return `@ban-duration=600;room-id=${ROOM_ID};target-user-id=${u.id};tmi-sent-ts=${Date.now()} :tmi.twitch.tv CLEARCHAT #${ch} :${u.login}`;
}

emoteNames = await loadEmoteNames();
makeUsers();
console.log(`fake IRC: ${emoteNames.length} emote names, ${users.length} users, RATE=${RATE}/s per channel, SEED=${SEED}`);

const wss = new WebSocketServer({ host: "127.0.0.1", port: PORT });
console.log(`listening on ws://127.0.0.1:${PORT}`);

wss.on("connection", (ws) => {
  const joined = new Set();
  let nick = "justinfan00000";
  const send = (l) => { if (ws.readyState === 1) ws.send(l); };
  let acc = 0;
  let last = Date.now();
  let nextMsg = Date.now() + 10000, nextSub = Date.now() + 20000, nextTimeout = Date.now() + 30000;

  const timer = setInterval(() => {
    const now = Date.now();
    acc += ((now - last) / 1000) * RATE;
    last = now;
    const n = Math.floor(acc);
    acc -= n;
    for (const ch of joined) {
      for (let i = 0; i < n; i++) send(privmsg(ch));
      if (now >= nextSub && chance(0.5)) send(usernotice(ch));
      if (now >= nextMsg) { const l = clearmsg(ch); if (l) send(l); }
      if (now >= nextTimeout) send(clearchat(ch));
    }
    if (now >= nextSub) nextSub = now + 15000 + rnd() * 20000;
    if (now >= nextMsg) nextMsg = now + 8000 + rnd() * 6000;
    if (now >= nextTimeout) nextTimeout = now + 30000;
    if (ws.bufferedAmount > 8_000_000) acc = 0; // slow consumer: do not pile up
  }, 50);

  ws.on("message", (data) => {
    for (const line of String(data).split(/\r?\n/)) {
      if (!line) continue;
      const [cmd, ...rest] = line.split(" ");
      if (cmd === "NICK") {
        nick = rest[0] || nick;
        send(`:tmi.twitch.tv 001 ${nick} :Welcome, GLHF!`);
        send(`:tmi.twitch.tv 002 ${nick} :Your host is tmi.twitch.tv`);
        send(`:tmi.twitch.tv 003 ${nick} :This server is rather new`);
        send(`:tmi.twitch.tv 004 ${nick} :-`);
        send(`:tmi.twitch.tv 375 ${nick} :-`);
        send(`:tmi.twitch.tv 372 ${nick} :You are in a maze of twisty passages.`);
        send(`:tmi.twitch.tv 376 ${nick} :>`);
      } else if (cmd === "CAP") {
        send(`:tmi.twitch.tv CAP * ACK :${line.split(":").slice(1).join(":") || "twitch.tv/tags twitch.tv/commands"}`);
      } else if (cmd === "JOIN") {
        for (const c of (rest[0] || "").split(",")) {
          const ch = c.replace(/^#/, "").toLowerCase();
          if (!ch) continue;
          joined.add(ch);
          send(`:${nick}!${nick}@${nick}.tmi.twitch.tv JOIN #${ch}`);
          send(`@emote-only=0;followers-only=-1;r9k=0;room-id=${ROOM_ID};slow=0;subs-only=0 :tmi.twitch.tv ROOMSTATE #${ch}`);
        }
      } else if (cmd === "PART") {
        joined.delete((rest[0] || "").replace(/^#/, "").toLowerCase());
      } else if (cmd === "PING") {
        send(`:tmi.twitch.tv PONG tmi.twitch.tv :${rest.join(" ").replace(/^:/, "")}`);
      }
    }
  });
  ws.on("close", () => clearInterval(timer));
  ws.on("error", () => clearInterval(timer));
});
