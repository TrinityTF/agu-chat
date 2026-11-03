// The mod / send HTTP calls from modtools.agu.lol web/src/app/pages/Chat.tsx (commit 1b826c2:
// handleDeleteMessage, resolveAutomod, pinMessage, unpinMessage, setPinDuration, rumbleModAction,
// useKickModAction), as functions over relayApi(source, ...) that report through a toast.
import { relayApi } from "../bridge";
import type { SourceRef } from "../protocol";
import type { Toast } from "./toast";
import type { KickMsg, RumbleMsg } from "./types";

async function failText(r: { status: number; text(): Promise<string> }, fallback: string): Promise<string> {
  const t = await r.text();
  // Bodies from the local relay may be JSON ({error}); strip to the message.
  let msg = t;
  try {
    const j = JSON.parse(t);
    if (j && typeof j === "object" && j.error) msg = String(j.error);
  } catch {
    /* plain text */
  }
  return r.status === 403 && msg === "relogin" ? "Log in again to grant mod permissions" : msg || fallback;
}

/** POST /api/mod/action (Twitch). Returns true on success. */
export async function twitchAction(source: SourceRef, body: Record<string, unknown>, ok: string | null, toast: Toast): Promise<boolean> {
  try {
    const r = await relayApi(source, "/api/mod/action", { method: "POST", body: JSON.stringify(body) });
    if (!r.ok) throw new Error(await failText(r, "action failed"));
    if (ok) toast(ok);
    return true;
  } catch (e) {
    toast((e as Error).message, "warn");
    return false;
  }
}

export const twitchDelete = (source: SourceRef, messageId: string, toast: Toast) =>
  twitchAction(source, { action: "delete", messageId }, null, toast);

// v5b item 2: resolve an AutoMod hold. On success the row is left alone: the authoritative
// "automodupdate" broadcast flips it. A 404 means Twitch already expired/resolved it, so the caller marks
// it locally (return "gone").
export async function resolveAutomod(source: SourceRef, id: string, action: "allow" | "deny", toast: Toast): Promise<"ok" | "gone" | "fail"> {
  try {
    const r = await relayApi(source, "/api/mod/automod", { method: "POST", body: JSON.stringify({ msgId: id, action }) });
    if (r.ok) return "ok";
    if (r.status === 404) {
      toast("Message is no longer held", "warn");
      return "gone";
    }
    toast(await failText(r, "action failed"), "warn");
    return "fail";
  } catch (e) {
    toast((e as Error).message, "warn");
    return "fail";
  }
}

export const pinMessage = (source: SourceRef, id: string, toast: Toast) =>
  twitchAction(source, { action: "pin", messageId: id, durationSeconds: 1200 }, "Pinned", toast);

export const unpinMessage = (source: SourceRef, id: string, toast: Toast) =>
  twitchAction(source, { action: "unpin", messageId: id }, "unpinned", toast);

export const setPinDuration = (source: SourceRef, id: string, seconds: number | null, toast: Toast) =>
  twitchAction(
    source,
    { action: "pinduration", messageId: id, ...(seconds != null ? { durationSeconds: seconds } : {}) },
    seconds == null ? "Pinned until unpinned or the stream ends" : "Pin time updated",
    toast,
  );

/** Rumble moderation targets the PERSON (`user.login`), not the channel a message was posted under. */
export async function rumbleModAction(source: SourceRef, action: "timeout" | "ban" | "delete", msg: RumbleMsg, toast: Toast, seconds = 300) {
  const who = msg.user.login || msg.user.name;
  if (action === "ban" && !window.confirm(`Ban ${who} from the Rumble chat?`)) return;
  try {
    const r = await relayApi(source, "/api/rumble/action", {
      method: "POST",
      body: JSON.stringify(action === "delete" ? { action, messageId: msg.id } : { action, username: who, ...(action === "timeout" ? { seconds } : {}) }),
    });
    if (!r.ok) throw new Error((await r.text()) || "Rumble rejected the action");
    toast(action === "timeout" ? `timed out ${who} on Rumble (${Math.round(seconds / 60)}m)` : action === "ban" ? `banned ${who} on Rumble` : "deleted on Rumble");
  } catch (e) {
    toast((e as Error).message, "warn");
  }
}

export type KickModAction = "timeout" | "ban" | "unban" | "delete";

export async function kickModAction(source: SourceRef, action: KickModAction, msg: KickMsg, toast: Toast, seconds?: number) {
  if (action === "ban" && !window.confirm(`Ban ${msg.user.name || "this user"} from the Kick chat?`)) return;
  try {
    const r = await relayApi(source, "/api/kick/action", {
      method: "POST",
      body: JSON.stringify({ action, userId: msg.user.id, messageId: msg.id, seconds }),
    });
    if (!r.ok) {
      toast("Kick: " + ((await r.text()) || "action failed"), "warn");
      return;
    }
    const who = msg.user.name || "user";
    if (action === "timeout") toast(`Timed out ${who} on Kick`);
    else if (action === "ban") toast(`Banned ${who} on Kick`);
    else if (action === "unban") toast(`Unbanned ${who} on Kick`);
    else toast("Message deleted on Kick");
  } catch (e) {
    toast("Kick: " + ((e as Error).message || "action failed"), "warn");
  }
}
