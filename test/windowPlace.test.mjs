import { test } from "node:test";
import assert from "node:assert/strict";
import { placeBounds } from "../src/main/windowPlace.js";

const primary = { x: 0, y: 0, width: 1920, height: 1040 };
const right = { x: 1920, y: 0, width: 2560, height: 1400 };

test("keeps a saved spot that is on a screen", () => {
  assert.deepEqual(placeBounds({ x: 100, y: 50, width: 420, height: 800 }, [primary]), { x: 100, y: 50, width: 420, height: 800 });
  assert.deepEqual(placeBounds({ x: 2200, y: 100, width: 420, height: 800 }, [primary, right]), { x: 2200, y: 100, width: 420, height: 800 });
});

test("a window left on an unplugged monitor comes back centered on the primary screen", () => {
  assert.deepEqual(placeBounds({ x: 2200, y: 100, width: 420, height: 800 }, [primary]), { x: 750, y: 120, width: 420, height: 800 });
});

test("a title bar above the top of the screen, or mostly off the side, is pulled back in", () => {
  assert.equal(placeBounds({ x: 100, y: -500, width: 420, height: 800 }, [primary]).y, 120);
  const side = placeBounds({ x: 1850, y: 50, width: 420, height: 800 }, [primary]);
  assert.equal(side.x, 750); // only 70px of title bar showed: not grabbable
  const partly = placeBounds({ x: 1700, y: 50, width: 420, height: 800 }, [primary]);
  assert.equal(partly.x, 1500); // reachable: just nudged fully on screen
});

test("too big for the screen shrinks to fit; nothing saved means the default spot", () => {
  assert.deepEqual(placeBounds({ x: 0, y: 0, width: 4000, height: 3000 }, [primary]), { x: 0, y: 0, width: 1920, height: 1040 });
  assert.equal(placeBounds(null, [primary]), null);
  assert.equal(placeBounds({ x: "a" }, [primary]), null);
});
