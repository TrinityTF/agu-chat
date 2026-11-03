import { test } from "node:test";
import assert from "node:assert/strict";
import { matchRow, parseSearch, searchRows } from "../src/renderer/chat/search.ts";
import type { Row } from "../src/renderer/chat/types.ts";

const msg = (text: string, login: string, name = login): Row =>
  ({
    key: `${login}-${text}`,
    ts: 1,
    sk: "twitch:chan",
    platform: "twitch",
    k: "msg",
    m: { platform: "twitch", id: text, ts: 1, user: { id: login, login, name, color: "" }, badges: "", text, emotes: "", action: false, firstMsg: false, reply: null, rewardId: null, msgId: null },
  }) as Row;
const feed = (text: string): Row => ({ key: `f-${text}`, ts: 1, sk: "twitch:chan", platform: "twitch", k: "modfeed", f: { ts: 1, feedKind: "ban", text } }) as Row;
const yt = (text: string, name: string): Row =>
  ({ key: `y-${text}`, ts: 1, sk: "youtube:c", platform: "youtube", k: "youtube", m: { platform: "youtube", id: text, ts: 1, user: { id: "y", name, avatar: "" }, badges: [], parts: [], text, paid: null, notice: null } }) as Row;

test("parseSearch splits words and from: prefixes", () => {
  assert.deepEqual(parseSearch("  Hello from:@Alice  world FROM:bob from: "), { from: ["alice", "bob"], words: ["hello", "world"] });
  assert.deepEqual(parseSearch(""), { from: [], words: [] });
});

test("all words must match text or username, case-insensitively", () => {
  const r = msg("Good Morning chat", "alice", "Alice_TV");
  assert.ok(matchRow(r, parseSearch("morning")));
  assert.ok(matchRow(r, parseSearch("GOOD chat")));
  assert.ok(matchRow(r, parseSearch("alice_tv morning")));
  assert.ok(!matchRow(r, parseSearch("morning evening")));
});

test("from: keeps only that author's rows (any of several)", () => {
  const rows = [msg("hi", "alice"), msg("hi", "bob"), msg("yo", "carol"), feed("alice was banned")];
  assert.deepEqual(searchRows(rows, "from:alice").map((r) => r.key), ["alice-hi"]);
  assert.deepEqual(searchRows(rows, "from:alice from:bob").map((r) => r.key), ["alice-hi", "bob-hi"]);
  assert.deepEqual(searchRows(rows, "from:carol hi").map((r) => r.key), []);
  assert.deepEqual(searchRows(rows, "banned").map((r) => r.key), ["f-alice was banned"]);
});

test("from: matches YouTube names with or without @", () => {
  assert.ok(matchRow(yt("hey", "@Streamer"), parseSearch("from:streamer")));
  assert.ok(matchRow(yt("hey", "@Streamer"), parseSearch("from:@streamer")));
});

test("an empty query returns the rows unchanged", () => {
  const rows = [msg("a", "x")];
  assert.equal(searchRows(rows, "   "), rows);
});
