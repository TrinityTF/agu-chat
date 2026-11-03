// STUB: replaced by the GitHub Releases updater. Keep this export shape.
import { EventEmitter } from "node:events";

export const events = new EventEmitter();
const state = { status: "dev", channel: "beta", version: null, notes: [], progress: 0, error: null };

export const getState = () => ({ ...state });
export function init(channel) {
  state.channel = channel;
}
export function setChannel(channel) {
  state.channel = channel;
}
export function check() {}
export async function install() {
  throw new Error("Updates are not available in development.");
}
