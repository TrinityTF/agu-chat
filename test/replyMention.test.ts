import { test } from "node:test";
import assert from "node:assert/strict";
import { dropReplyMention } from "../src/renderer/chat/twitch/replyMention.ts";
import type { MsgPart } from "../src/renderer/chat/twitch/parts.ts";

const text = (t: string): MsgPart => ({ kind: "text", text: t });

test("drops a leading @parent written as plain text", () => {
  assert.deepEqual(dropReplyMention([text("@TheLadyCupcake whoever wins")], "TheLadyCupcake"), [text("whoever wins")]);
  assert.deepEqual(dropReplyMention([text("@theladycupcake, ok")], "TheLadyCupcake"), [text("ok")]);
});

test("drops a leading mention part and the space after it", () => {
  const parts: MsgPart[] = [{ kind: "mention", login: "a_g_u__", name: "A_g_u__", color: "#fff", userId: "1" }, text(" hi")];
  assert.deepEqual(dropReplyMention(parts, "A_g_u__"), [text("hi")]);
});

test("keeps the text when it starts with someone else, or the mention is all there is", () => {
  assert.deepEqual(dropReplyMention([text("@someone else")], "TheLadyCupcake"), [text("@someone else")]);
  const emote: MsgPart = { kind: "emote", source: "twitch", id: "1", name: "Kappa", overlays: [] };
  assert.deepEqual(dropReplyMention([text("@Bob "), emote], "bob"), [emote]);
});
