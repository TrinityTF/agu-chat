import { test } from "node:test";
import assert from "node:assert/strict";
import {
  DEFAULT_MOD_BUTTONS,
  MAX_MOD_BUTTONS,
  describeModButton,
  expandTemplate,
  formatDuration,
  normalizeModButton,
  normalizeModButtons,
  parseDuration,
} from "../src/renderer/chat/modButtons.ts";

test("expandTemplate fills every placeholder", () => {
  const vars = { user: "bob", name: "Bob", userId: "123", msgId: "m-1", channel: "agu", msg: "hello there" };
  assert.equal(expandTemplate("/timeout {user} 600", vars), "/timeout bob 600");
  assert.equal(expandTemplate("/ban {user} spam in {channel}", vars), "/ban bob spam in agu");
  assert.equal(expandTemplate("/delete {msg-id}", vars), "/delete m-1");
  assert.equal(expandTemplate("@{name} ({user-id}) said: {msg}", vars), "@Bob (123) said: hello there");
  assert.equal(expandTemplate("{USER} {User}", vars), "bob bob");
  assert.equal(expandTemplate("{user}{user}", vars), "bobbob");
});

test("expandTemplate leaves unknown or unavailable placeholders alone and does not re-expand values", () => {
  assert.equal(expandTemplate("/warn {user} {reason}", { user: "bob" }), "/warn bob {reason}");
  assert.equal(expandTemplate("/delete {msg-id}", { user: "bob" }), "/delete {msg-id}");
  assert.equal(expandTemplate("{name}", { user: "bob" }), "bob"); // name falls back to the login
  assert.equal(expandTemplate("say {msg}", { user: "x", msg: "{user}\nline2" }), "say {user} line2");
  assert.equal(expandTemplate("{ user } {}", { user: "bob" }), "{ user } {}");
});

test("parseDuration and formatDuration", () => {
  assert.equal(parseDuration("600"), 600);
  assert.equal(parseDuration("10m"), 600);
  assert.equal(parseDuration("1H"), 3600);
  assert.equal(parseDuration("2 d"), 172800);
  assert.equal(parseDuration("1w"), 604800);
  assert.equal(parseDuration("99w"), 1_209_600); // Twitch's 2-week cap
  assert.equal(parseDuration("0"), 1);
  assert.equal(parseDuration("ten"), null);
  assert.equal(parseDuration("1.5m"), null);
  assert.equal(formatDuration(600), "10m");
  assert.equal(formatDuration(90), "90s");
  assert.equal(formatDuration(86400), "1d");
  assert.equal(formatDuration(1_209_600), "2w");
  assert.equal(formatDuration(5400), "90m");
});

test("defaults are ban / timeout 10m / warn / delete", () => {
  assert.deepEqual(
    DEFAULT_MOD_BUTTONS.map((b) => [b.action, b.seconds]),
    [
      ["ban", 0],
      ["timeout", 600],
      ["warn", 0],
      ["delete", 0],
    ],
  );
  assert.deepEqual(DEFAULT_MOD_BUTTONS.map(describeModButton), ["Ban", "Timeout 10m", "Warn", "Delete message"]);
  assert.equal(normalizeModButtons(undefined), DEFAULT_MOD_BUTTONS);
  assert.equal(normalizeModButtons({}), DEFAULT_MOD_BUTTONS);
  assert.deepEqual(normalizeModButtons(DEFAULT_MOD_BUTTONS), DEFAULT_MOD_BUTTONS);
  assert.deepEqual(normalizeModButtons([]), []); // an empty list is the user's choice
});

test("normalizeModButton validates and fills in", () => {
  assert.equal(normalizeModButton(null), null);
  assert.equal(normalizeModButton({ action: "explode" }), null);
  assert.equal(normalizeModButton({ id: "c", action: "command", command: "   " }), null);
  assert.deepEqual(normalizeModButton({ id: "t", action: "timeout" }), { id: "t", action: "timeout", icon: "clock", label: "", seconds: 600, command: "" });
  assert.equal(normalizeModButton({ id: "t", action: "timeout", seconds: 9e9 })?.seconds, 1_209_600);
  assert.deepEqual(normalizeModButton({ id: "b", action: "ban", icon: "nope", label: "PERMA-BAN!", seconds: 5, command: "x" }), {
    id: "b",
    action: "ban",
    icon: "",
    label: "PERMA-BA",
    seconds: 0,
    command: "",
  });
  assert.equal(normalizeModButton({ id: "c", action: "command", command: "/warn {user} rules", icon: "warning" })?.command, "/warn {user} rules");
  assert.ok(normalizeModButton({ action: "delete" })?.id); // missing id gets one
});

test("normalizeModButtons drops bad entries, fixes duplicate ids and caps the count", () => {
  const list = normalizeModButtons([{ id: "a", action: "ban" }, "junk", { id: "a", action: "delete" }, { action: "nope" }]);
  assert.deepEqual(
    list.map((b) => b.action),
    ["ban", "delete"],
  );
  assert.notEqual(list[0].id, list[1].id);
  const many = normalizeModButtons(Array.from({ length: 30 }, (_, i) => ({ id: `x${i}`, action: "delete" })));
  assert.equal(many.length, MAX_MOD_BUTTONS);
});
