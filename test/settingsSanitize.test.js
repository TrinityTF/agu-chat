import { test } from "node:test";
import assert from "node:assert/strict";
import { loadSettings, sanitizeSetting } from "../src/main/settingsSanitize.js";

const DEFAULTS = { fontSize: 13, updateChannel: "beta", timestamps: true, ignoredUsers: [], ignoredPhrases: [] };

test("scalars keep the default's type; strings are trimmed", () => {
  assert.equal(sanitizeSetting("fontSize", 15, DEFAULTS), 15);
  assert.equal(sanitizeSetting("fontSize", "15", DEFAULTS), undefined);
  assert.equal(sanitizeSetting("updateChannel", " stable ", DEFAULTS), "stable");
  assert.equal(sanitizeSetting("nope", 1, DEFAULTS), undefined);
  assert.equal(sanitizeSetting("timestamps", null, DEFAULTS), undefined);
});

test("ignoredUsers: arrays only, entries normalized and de-duplicated", () => {
  assert.equal(sanitizeSetting("ignoredUsers", { platform: "twitch", login: "a" }, DEFAULTS), undefined);
  assert.equal(sanitizeSetting("ignoredUsers", "a", DEFAULTS), undefined);
  assert.deepEqual(
    sanitizeSetting("ignoredUsers", [{ platform: "twitch", login: " @Alice " }, { platform: "twitch", login: "alice" }, { platform: "nope", login: "x" }, { platform: "kick", login: "" }, null, "x", { platform: "kick", login: "Bob" }], DEFAULTS),
    [
      { platform: "twitch", login: "alice" },
      { platform: "kick", login: "bob" },
    ],
  );
});

test("ignoredPhrases: entries get every field with a safe value", () => {
  assert.deepEqual(
    sanitizeSetting("ignoredPhrases", [{ pattern: "spam" }, { pattern: "  " }, { pattern: "a+", regex: true, caseSensitive: true, action: "replace", replacement: "b", extra: 1 }, { pattern: 5 }], DEFAULTS),
    [
      { pattern: "spam", regex: false, caseSensitive: false, action: "hide", replacement: "" },
      { pattern: "a+", regex: true, caseSensitive: true, action: "replace", replacement: "b" },
    ],
  );
});

test("a pattern keeps its spaces (they can matter)", () => {
  assert.equal(sanitizeSetting("ignoredPhrases", [{ pattern: " hi " }], DEFAULTS)[0].pattern, " hi ");
});

test("loadSettings overlays valid stored keys on the defaults", () => {
  const s = loadSettings({ fontSize: 16, timestamps: "yes", ignoredUsers: "bad", ignoredPhrases: [{ pattern: "x" }], junk: 1 }, DEFAULTS);
  assert.equal(s.fontSize, 16);
  assert.equal(s.timestamps, true);
  assert.deepEqual(s.ignoredUsers, []);
  assert.equal(s.ignoredPhrases.length, 1);
  assert.ok(!("junk" in s));
  assert.deepEqual(loadSettings(null, DEFAULTS), DEFAULTS);
});
