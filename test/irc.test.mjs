import { test } from "node:test";
import assert from "node:assert/strict";
import { parseIrcLine } from "../src/main/twitch/vendor/irc.js";

test("reads the trailing text after ' :'", () => {
  const m = parseIrcLine("@id=1 :bob!bob@bob.tmi.twitch.tv PRIVMSG #chan :hello there");
  assert.equal(m.command, "PRIVMSG");
  assert.equal(m.channel, "chan");
  assert.equal(m.user, "bob");
  assert.equal(m.text, "hello there");
});

test("reads a one-word message sent without the ':' (recent-messages does this)", () => {
  const m = parseIrcLine("@emote-only=1;id=2 :silveraku!silveraku@silveraku.tmi.twitch.tv PRIVMSG #zackrawrr anthor6Birbviolence");
  assert.equal(m.text, "anthor6Birbviolence");
  assert.equal(m.channel, "zackrawrr");
});

test("lines with no message stay empty", () => {
  assert.equal(parseIrcLine(":bob!bob@bob.tmi.twitch.tv JOIN #chan").text, "");
});
