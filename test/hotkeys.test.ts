import { test } from "node:test";
import assert from "node:assert/strict";
import {
  HOTKEY_ACTIONS,
  bindingMap,
  eventToBinding,
  findConflicts,
  isUsableBinding,
  normalizeBinding,
  parseBinding,
  resolveHotkeys,
  toOverrides,
} from "../src/renderer/layout/hotkeys.ts";

const ev = (key: string, code: string, mods: { ctrl?: boolean; alt?: boolean; shift?: boolean; meta?: boolean } = {}) => ({
  key,
  code,
  ctrlKey: !!mods.ctrl,
  altKey: !!mods.alt,
  shiftKey: !!mods.shift,
  metaKey: !!mods.meta,
});

test("parseBinding reads modifiers and keys in any case and order", () => {
  assert.deepEqual(parseBinding("ctrl+shift+w"), { ctrl: true, alt: false, shift: true, key: "W" });
  assert.deepEqual(parseBinding("Shift+Ctrl+W"), { ctrl: true, alt: false, shift: true, key: "W" });
  assert.deepEqual(parseBinding("Alt+F4"), { ctrl: false, alt: true, shift: false, key: "F4" });
  assert.deepEqual(parseBinding("Ctrl+pageup"), { ctrl: true, alt: false, shift: false, key: "PageUp" });
  assert.deepEqual(parseBinding("Cmd+1"), { ctrl: true, alt: false, shift: false, key: "1" });
  assert.deepEqual(parseBinding(" Ctrl + Tab "), { ctrl: true, alt: false, shift: false, key: "Tab" });
  assert.deepEqual(parseBinding("Ctrl+Esc"), { ctrl: true, alt: false, shift: false, key: "Escape" });
});

test("parseBinding rejects malformed bindings", () => {
  for (const bad of ["", "Ctrl+", "+W", "Ctrl+Ctrl+W", "Hyper+W", "Ctrl+Shift", "Ctrl+WW", "Ctrl+F25", "Ctrl++"]) {
    assert.equal(parseBinding(bad), null, bad);
  }
  assert.equal(parseBinding(42 as unknown as string), null);
});

test("normalizeBinding gives one canonical spelling", () => {
  assert.equal(normalizeBinding("shift+alt+ctrl+k"), "Ctrl+Alt+Shift+K");
  assert.equal(normalizeBinding("ctrl+pgdn"), "Ctrl+PageDown");
  assert.equal(normalizeBinding("f5"), "F5");
  assert.equal(normalizeBinding("nope+x"), null);
});

test("isUsableBinding needs Ctrl or Alt unless it is a function key", () => {
  assert.equal(isUsableBinding("Ctrl+T"), true);
  assert.equal(isUsableBinding("Alt+1"), true);
  assert.equal(isUsableBinding("F2"), true);
  assert.equal(isUsableBinding("T"), false);
  assert.equal(isUsableBinding("Shift+T"), false);
  assert.equal(isUsableBinding("Tab"), false);
});

test("eventToBinding uses physical keys and treats Cmd as Ctrl", () => {
  assert.equal(eventToBinding(ev("w", "KeyW", { ctrl: true })), "Ctrl+W");
  assert.equal(eventToBinding(ev("W", "KeyW", { ctrl: true, shift: true })), "Ctrl+Shift+W");
  assert.equal(eventToBinding(ev("!", "Digit1", { ctrl: true, shift: true })), "Ctrl+Shift+1");
  assert.equal(eventToBinding(ev("Tab", "Tab", { ctrl: true, shift: true })), "Ctrl+Shift+Tab");
  assert.equal(eventToBinding(ev("PageDown", "PageDown", { ctrl: true })), "Ctrl+PageDown");
  assert.equal(eventToBinding(ev("t", "KeyT", { meta: true })), "Ctrl+T");
  assert.equal(eventToBinding(ev("_", "Minus", { ctrl: true, shift: true })), "Ctrl+Shift+-");
  assert.equal(eventToBinding(ev(" ", "Space", { alt: true })), "Alt+Space");
  assert.equal(eventToBinding(ev("Control", "ControlLeft", { ctrl: true })), null);
  assert.equal(eventToBinding(ev("Shift", "ShiftLeft", { shift: true })), null);
});

test("defaults match the original shortcuts", () => {
  const t = resolveHotkeys(undefined);
  assert.deepEqual(t.newTab, ["Ctrl+T"]);
  assert.deepEqual(t.closeSplit, ["Ctrl+W"]);
  assert.deepEqual(t.closeTab, ["Ctrl+Shift+W"]);
  assert.deepEqual(t.nextTab, ["Ctrl+Tab", "Ctrl+PageDown"]);
  assert.deepEqual(t.prevTab, ["Ctrl+Shift+Tab", "Ctrl+PageUp"]);
  assert.deepEqual(t.selectTab1, ["Ctrl+1"]);
  assert.deepEqual(t.selectLastTab, ["Ctrl+9"]);
  assert.deepEqual(t.openSettings, ["Ctrl+P"]);
  assert.equal(findConflicts(t).size, 0);
});

test("resolveHotkeys applies overrides and drops junk", () => {
  const t = resolveHotkeys({ newTab: ["ctrl+n", "ctrl+N", "Q", 7, "bogus+x"], closeSplit: [], madeUp: ["Ctrl+M"], prevTab: "Ctrl+Q" });
  assert.deepEqual(t.newTab, ["Ctrl+N"]); // normalized, deduped, unusable/invalid dropped
  assert.deepEqual(t.closeSplit, []); // explicitly unbound
  assert.deepEqual(t.prevTab, ["Ctrl+Shift+Tab", "Ctrl+PageUp"]); // not a list: default
  assert.equal("madeUp" in t, false);
  assert.deepEqual(resolveHotkeys([]).newTab, ["Ctrl+T"]);
  assert.deepEqual(resolveHotkeys("x").newTab, ["Ctrl+T"]);
});

test("toOverrides keeps only the changed actions and round-trips", () => {
  const t = resolveHotkeys({});
  assert.deepEqual(toOverrides(t), {});
  const changed = { ...t, newTab: ["Ctrl+N"], closeSplit: [] };
  const o = toOverrides(changed);
  assert.deepEqual(o, { newTab: ["Ctrl+N"], closeSplit: [] });
  assert.deepEqual(resolveHotkeys(o), changed);
});

test("bindingMap and findConflicts", () => {
  const t = resolveHotkeys({ newTab: ["Ctrl+W"] });
  const m = bindingMap(t);
  assert.equal(m.get("Ctrl+Tab"), "nextTab");
  assert.equal(m.get("Ctrl+9"), "selectLastTab");
  // The earlier action in the list wins a shared binding.
  const first = HOTKEY_ACTIONS.findIndex((a) => a.id === "newTab") < HOTKEY_ACTIONS.findIndex((a) => a.id === "closeSplit") ? "newTab" : "closeSplit";
  assert.equal(m.get("Ctrl+W"), first);
  assert.deepEqual(findConflicts(t).get("Ctrl+W")?.sort(), ["closeSplit", "newTab"]);
});
