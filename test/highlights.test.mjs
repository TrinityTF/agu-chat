// Highlight rule matching (src/renderer/chat/highlights.ts). Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { compileHighlights, matchInputOf, nextRuleColor, regexError, ruleRowStyle, ruleUsers, tint } from "../src/renderer/chat/highlights.ts";

let n = 0;
const rule = (over) => ({
  id: `r${++n}`,
  enabled: true,
  kind: "phrase",
  pattern: "",
  caseSensitive: false,
  color: "#ff9f1a",
  sound: false,
  soundFile: "",
  flash: false,
  ...over,
});
const msg = (text, login = "someone", name = login) => ({ text, login, name });

test("phrase matches whole words, case-insensitive by default", () => {
  const h = compileHighlights([rule({ pattern: "agu" })]);
  assert.ok(h.match(msg("hi agu!")));
  assert.ok(h.match(msg("AGU is here")));
  assert.ok(h.match(msg("agu")));
  assert.equal(h.match(msg("agurk salad")), null);
  assert.equal(h.match(msg("magu")), null);
  assert.equal(h.match(msg("my_agu")), null);
});

test("phrase case sensitivity toggle", () => {
  const h = compileHighlights([rule({ pattern: "Drop", caseSensitive: true })]);
  assert.ok(h.match(msg("Drop incoming")));
  assert.equal(h.match(msg("drop incoming")), null);
});

test("phrase with spaces, punctuation and regex characters is literal", () => {
  const h = compileHighlights([rule({ pattern: "!drop (now)" })]);
  assert.ok(h.match(msg("type !drop (now) please")));
  assert.equal(h.match(msg("type !drop now please")), null);
  const dots = compileHighlights([rule({ pattern: "a.b" })]);
  assert.equal(dots.match(msg("axb")), null);
  assert.ok(dots.match(msg("see a.b")));
});

test("phrase word boundaries work for non-latin scripts", () => {
  const h = compileHighlights([rule({ pattern: "привет" })]);
  assert.ok(h.match(msg("ну привет всем")));
  assert.equal(h.match(msg("приветствую")), null);
});

test("regex rules, with and without case sensitivity", () => {
  const ci = compileHighlights([rule({ kind: "regex", pattern: "^gg+$" })]);
  assert.ok(ci.match(msg("GGGG")));
  assert.equal(ci.match(msg("ggez")), null);
  const cs = compileHighlights([rule({ kind: "regex", pattern: "W+", caseSensitive: true })]);
  assert.ok(cs.match(msg("WWW")));
  assert.equal(cs.match(msg("www")), null);
});

test("an invalid regex never matches and is reported", () => {
  const h = compileHighlights([rule({ kind: "regex", pattern: "(unclosed" })]);
  assert.equal(h.size, 0);
  assert.equal(h.match(msg("(unclosed")), null);
  assert.ok(regexError("(unclosed", false));
  assert.equal(regexError("a+b", false), null);
});

test("user rules match login or display name, comma or space separated, @ ignored", () => {
  const h = compileHighlights([rule({ kind: "user", pattern: "@Alice, bob  carol" })]);
  assert.ok(h.match(msg("hello", "alice", "Alice")));
  assert.ok(h.match(msg("hello", "bob_123", "BOB")));
  assert.ok(h.match(msg("hello", "", "Carol")));
  assert.equal(h.match(msg("alice said hi", "dave")), null, "user rules look at the author, not the text");
  assert.deepEqual(ruleUsers(" @a, b c,,"), ["a", "b", "c"]);
});

test("user rules can be case-sensitive", () => {
  const h = compileHighlights([rule({ kind: "user", pattern: "Alice", caseSensitive: true })]);
  assert.ok(h.match(msg("x", "nope", "Alice")));
  assert.equal(h.match(msg("x", "alice", "alice")), null);
});

test("disabled and empty rules are skipped; first match in list order wins", () => {
  const off = rule({ pattern: "hello", enabled: false, color: "#000000" });
  const empty = rule({ pattern: "   " });
  const a = rule({ pattern: "hello", color: "#111111" });
  const b = rule({ kind: "regex", pattern: "hel+o", color: "#222222" });
  const h = compileHighlights([off, empty, a, b]);
  assert.equal(h.size, 2);
  assert.equal(h.match(msg("hello there")), a);
  assert.equal(compileHighlights([b, a]).match(msg("hello there")), b);
  assert.equal(compileHighlights(undefined).match(msg("hello")), null);
});

test("matchInputOf reads every platform's rows and skips system rows", () => {
  const twitch = { key: "k", ts: 1, sk: "twitch:a", platform: "twitch", k: "msg", m: { text: "hi", user: { id: "1", login: "al", name: "Al", color: "" } } };
  assert.deepEqual(matchInputOf(twitch), { text: "hi", login: "al", name: "Al" });
  const kick = { key: "k", ts: 1, sk: "kick:a", platform: "kick", k: "kick", m: { text: "gg [emote:37226:KEKW] lol", user: { login: "bo", name: "Bo" } } };
  assert.deepEqual(matchInputOf(kick), { text: "gg KEKW lol", login: "bo", name: "Bo" });
  const yt = { key: "k", ts: 1, sk: "youtube:a", platform: "youtube", k: "youtube", m: { text: "yo", user: { id: "c", name: "Cy" } } };
  assert.deepEqual(matchInputOf(yt), { text: "yo", login: "", name: "Cy" });
  const feed = { key: "k", ts: 1, sk: "twitch:a", platform: "twitch", k: "modfeed", f: { ts: 1, feedKind: "ban", text: "x banned" } };
  assert.equal(matchInputOf(feed), null);
  const kickRule = compileHighlights([rule({ pattern: "KEKW" })]);
  assert.ok(kickRule.match(matchInputOf(kick)));
});

test("colours: tint, row style and the next unused colour", () => {
  assert.equal(tint("#ff0080", 0.5), "rgba(255,0,128,0.5)");
  assert.equal(tint("nope", 1), "rgba(145,71,255,1)");
  assert.equal(ruleRowStyle("#000000").borderLeft, "3px solid rgba(0,0,0,1)");
  assert.equal(nextRuleColor([]), "#ff9f1a");
  assert.equal(nextRuleColor([rule({ color: "#FF9F1A" })]), "#2bd4a4");
});
