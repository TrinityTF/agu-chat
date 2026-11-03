// Settings validation for highlight rules (src/main/highlightRules.js). Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { MAX_PATTERN, MAX_RULES, isSoundFileName, sanitizeHighlightRule, sanitizeHighlightRules } from "../src/main/highlightRules.js";

test("a list is required", () => {
  assert.equal(sanitizeHighlightRules("nope"), undefined);
  assert.equal(sanitizeHighlightRules({}), undefined);
  assert.equal(sanitizeHighlightRules(null), undefined);
  assert.deepEqual(sanitizeHighlightRules([]), []);
});

test("a good rule passes through unchanged", () => {
  const r = { id: "a", enabled: true, kind: "regex", pattern: "^hi", caseSensitive: true, color: "#ff9f1a", sound: true, soundFile: "ping-1a2b3c4d.mp3", flash: true };
  assert.deepEqual(sanitizeHighlightRule(r), r);
});

test("bad fields get safe defaults", () => {
  const r = sanitizeHighlightRule({ id: "b", kind: "eval", pattern: 5, color: "red", soundFile: "../../secrets.bin", sound: "yes", enabled: 0 });
  assert.deepEqual(r, { id: "b", enabled: true, kind: "phrase", pattern: "", caseSensitive: false, color: "#9147ff", sound: false, soundFile: "", flash: false });
  assert.equal(sanitizeHighlightRule({ id: "c", enabled: false }).enabled, false);
  assert.equal(sanitizeHighlightRule({ id: "d", color: "#ABCDEF" }).color, "#abcdef");
  assert.equal(sanitizeHighlightRule({ id: "e", pattern: "x".repeat(MAX_PATTERN + 10) }).pattern.length, MAX_PATTERN);
});

test("entries without an id, non-objects and duplicate ids are dropped", () => {
  const out = sanitizeHighlightRules([{ id: "a" }, null, "x", [], { pattern: "no id" }, { id: "a", pattern: "dupe" }, { id: "b" }]);
  assert.deepEqual(out.map((r) => r.id), ["a", "b"]);
});

test("the list is capped", () => {
  const many = Array.from({ length: MAX_RULES + 5 }, (_, i) => ({ id: String(i) }));
  assert.equal(sanitizeHighlightRules(many).length, MAX_RULES);
});

test("sound file names are bare names inside the sounds folder", () => {
  assert.ok(isSoundFileName("my sound-1a2b.mp3"));
  assert.equal(isSoundFileName("../x.mp3"), false);
  assert.equal(isSoundFileName("a/b.mp3"), false);
  assert.equal(isSoundFileName("a\\b.mp3"), false);
  assert.equal(isSoundFileName(".hidden.mp3"), false);
  assert.equal(isSoundFileName(""), false);
  assert.equal(isSoundFileName(3), false);
});
