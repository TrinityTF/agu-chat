import { test } from "node:test";
import assert from "node:assert/strict";
import { tabWindow } from "../src/renderer/layout/tabWindow.ts";

const w = (n: number) => Array(n).fill(100);

test("everything that fits from the start, the selected tab included", () => {
  assert.deepEqual(tabWindow(w(6), 350, 1, 0), { start: 0, end: 2 });
});

test("the run stays put while the selected tab is inside it", () => {
  assert.deepEqual(tabWindow(w(6), 350, 3, 2), { start: 2, end: 4 });
});

test("selecting past the right end drops tabs off the left", () => {
  assert.deepEqual(tabWindow(w(6), 350, 4, 0), { start: 2, end: 4 });
});

test("selecting before the run starts it there", () => {
  assert.deepEqual(tabWindow(w(6), 350, 1, 3), { start: 1, end: 3 });
});

test("leftover room pulls hidden tabs back in from the left", () => {
  assert.deepEqual(tabWindow(w(4), 350, 3, 3), { start: 1, end: 3 });
});

test("a selected tab wider than the room still shows, alone", () => {
  assert.deepEqual(tabWindow([100, 500, 100], 300, 1, 0), { start: 1, end: 1 });
});
