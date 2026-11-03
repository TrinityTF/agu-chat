import { test } from "node:test";
import assert from "node:assert/strict";
import {
  applyIgnores,
  compileIgnores,
  filterIgnored,
  findEdits,
  applyEdits,
  ignoreLogin,
  isUserIgnored,
  phraseError,
  remapEmoteTag,
  toggleIgnoredUser,
} from "../src/renderer/chat/ignores.ts";
import type { IgnoredPhrase } from "../src/renderer/protocol.ts";
import type { Row } from "../src/renderer/chat/types.ts";

const twitch = (text: string, login = "alice", extra: Record<string, unknown> = {}): Row =>
  ({
    key: `t-${text}`,
    ts: 1,
    sk: "twitch:chan",
    platform: "twitch",
    k: "msg",
    m: { platform: "twitch", id: "1", ts: 1, user: { id: "u1", login, name: login, color: "" }, badges: "", text, emotes: "", action: false, firstMsg: false, reply: null, rewardId: null, msgId: null, ...extra },
  }) as Row;

const kick = (text: string, login = "bob"): Row =>
  ({ key: `k-${text}`, ts: 1, sk: "kick:chan", platform: "kick", k: "kick", m: { platform: "kick", id: "2", ts: 1, user: { id: "9", login, name: login, color: "", avatar: "", verified: false, anonymous: false }, badges: [], text, reply: null, action: false, notice: null } }) as Row;

const youtube = (name: string, parts: { t: "text"; v: string }[]): Row =>
  ({ key: "y", ts: 1, sk: "youtube:chan", platform: "youtube", k: "youtube", m: { platform: "youtube", id: "3", ts: 1, user: { id: "yt1", name, avatar: "" }, badges: [], parts, text: parts.map((p) => p.v).join(""), paid: null, notice: null } }) as Row;

const phrase = (p: Partial<IgnoredPhrase>): IgnoredPhrase => ({ pattern: "", regex: false, caseSensitive: false, action: "hide", replacement: "", ...p });

test("ignored users hide that user's messages on that platform only", () => {
  const c = compileIgnores([{ platform: "twitch", login: "alice" }], []);
  assert.equal(applyIgnores(twitch("hi", "alice"), c), null);
  assert.equal(applyIgnores(twitch("hi", "ALICE"), c), null);
  const other = twitch("hi", "carol");
  assert.equal(applyIgnores(other, c), other);
  const kickAlice = kick("hi", "alice");
  assert.equal(applyIgnores(kickAlice, c), kickAlice);
});

test("YouTube users are ignored by name", () => {
  const c = compileIgnores([{ platform: "youtube", login: "spammer" }], []);
  assert.equal(applyIgnores(youtube("@Spammer", [{ t: "text", v: "x" }]), c), null);
});

test("notices and mod lines are never hidden", () => {
  const c = compileIgnores([{ platform: "twitch", login: "alice" }], [phrase({ pattern: "sub" })]);
  const notice = { key: "n", ts: 1, sk: "twitch:chan", platform: "twitch", k: "notice", n: { id: "n", ts: 1, system: "alice subscribed", msgId: "sub", user: { id: "u1", login: "alice", name: "alice", color: "" }, text: "sub" } } as Row;
  assert.equal(applyIgnores(notice, c), notice);
});

test("hide phrases: plain is case-insensitive by default and literal", () => {
  const c = compileIgnores([], [phrase({ pattern: "a.b" })]);
  assert.equal(applyIgnores(twitch("look A.B here"), c), null);
  const r = twitch("aXb");
  assert.equal(applyIgnores(r, c), r);
});

test("hide phrases: case sensitive and regex", () => {
  const cs = compileIgnores([], [phrase({ pattern: "Spam", caseSensitive: true })]);
  const lower = twitch("spam");
  assert.equal(applyIgnores(lower, cs), lower);
  assert.equal(applyIgnores(twitch("Spam"), cs), null);
  const re = compileIgnores([], [phrase({ pattern: "^!\\w+", regex: true })]);
  assert.equal(applyIgnores(twitch("!command"), re), null);
  const notCmd = twitch("not !command");
  assert.equal(applyIgnores(notCmd, re), notCmd);
});

test("invalid or empty regexes are skipped and reported", () => {
  assert.ok(phraseError({ pattern: "(", regex: true, caseSensitive: false }));
  assert.equal(phraseError({ pattern: "(", regex: false, caseSensitive: false }), null);
  const c = compileIgnores([], [phrase({ pattern: "(", regex: true }), phrase({ pattern: "x*", regex: true })]);
  assert.equal(c.empty, false);
  const r = twitch("hello");
  assert.equal(applyIgnores(r, c), r); // x* only matches empty strings here: not a hit
});

test("replace phrases rewrite the text", () => {
  const c = compileIgnores([], [phrase({ pattern: "darn", action: "replace", replacement: "***" })]);
  const out = applyIgnores(kick("Darn it, darn"), c);
  assert.equal(out && out.k === "kick" && out.m.text, "*** it, ***");
});

test("replace rules are matched once against the original text, earliest and longest first", () => {
  const c = compileIgnores([], [phrase({ pattern: "ab", action: "replace", replacement: "1" }), phrase({ pattern: "abc", action: "replace", replacement: "2" }), phrase({ pattern: "1", action: "replace", replacement: "X" })]);
  const edits = findEdits("abc ab", c.replace);
  assert.equal(applyEdits("abc ab", edits), "2 1");
});

test("replacing keeps Twitch emote ranges that the edit did not touch, shifted", () => {
  // "Kappa darn Kappa": emote 25 at 0-4 and 11-15 (code points).
  const c = compileIgnores([], [phrase({ pattern: "darn", action: "replace", replacement: "x" })]);
  const out = applyIgnores(twitch("Kappa darn Kappa", "alice", { emotes: "25:0-4,11-15" }), c);
  assert.ok(out && out.k === "msg");
  assert.equal(out.m.text, "Kappa x Kappa");
  assert.equal(out.m.emotes, "25:0-4,8-12");
});

test("emote ranges inside a replaced span are dropped; code points are used", () => {
  const c = compileIgnores([], [phrase({ pattern: "Kappa", action: "replace", replacement: "" })]);
  const out = applyIgnores(twitch("😀 Kappa LUL", "alice", { emotes: "25:2-6/425618:8-10" }), c);
  assert.ok(out && out.k === "msg");
  assert.equal(out.m.text, "😀  LUL");
  assert.equal(out.m.emotes, "425618:3-5");
  assert.equal(Array.from(out.m.text).slice(3, 6).join(""), "LUL");
});

test("remapEmoteTag drops ids with no ranges left", () => {
  assert.equal(remapEmoteTag("1:0-1/2:3-4", (s, e) => (s === 0 ? null : [s, e])), "2:3-4");
});

test("YouTube replacement works on text parts", () => {
  const c = compileIgnores([], [phrase({ pattern: "bad", action: "replace", replacement: "good" })]);
  const out = applyIgnores(youtube("x", [{ t: "text", v: "so bad" }]), c);
  assert.ok(out && out.k === "youtube");
  assert.deepEqual(out.m.parts, [{ t: "text", v: "so good" }]);
  assert.equal(out.m.text, "so good");
});

test("filterIgnored keeps untouched rows identical and caches results", () => {
  const c = compileIgnores([{ platform: "twitch", login: "alice" }], [phrase({ pattern: "x", action: "replace", replacement: "y" })]);
  const a = twitch("hi", "alice");
  const b = twitch("hello", "carol");
  const d = twitch("x", "carol");
  const cache = new WeakMap();
  const out = filterIgnored([a, b, d], c, cache);
  assert.equal(out.length, 2);
  assert.equal(out[0], b);
  assert.equal(out[1].k === "msg" && out[1].m.text, "y");
  assert.equal(filterIgnored([a, b, d], c, cache)[1], out[1]);
  const none = [a];
  assert.equal(filterIgnored(none, compileIgnores([], []), new WeakMap()), none);
});

test("toggle / check ignored users normalizes logins", () => {
  assert.equal(ignoreLogin({ login: "", name: "@Some One" }), "some one");
  let list = toggleIgnoredUser([], "twitch", "@Alice", true);
  assert.deepEqual(list, [{ platform: "twitch", login: "alice" }]);
  assert.ok(isUserIgnored(list, "twitch", "ALICE"));
  assert.ok(!isUserIgnored(list, "kick", "alice"));
  list = toggleIgnoredUser(list, "twitch", "alice", true);
  assert.equal(list.length, 1);
  list = toggleIgnoredUser(list, "twitch", "alice", false);
  assert.deepEqual(list, []);
});
