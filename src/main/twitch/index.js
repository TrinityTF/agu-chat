// Twitch platform module. See docs/ARCHITECTURE.md ("Platform module").
//
//   reading   anonymous IRC (one shared socket, ircHub.js) + recent-messages history (backlog.js)
//   mod feed  one EventSub session (eventsub.js) for the channels the user moderates
//   writing   Helix with the user's own token (routes.js)
//   login     OAuth device code flow with the user's own Public client id (auth.js)
//
// Nothing here talks to agu.lol / api.agu.lol, and there is no bot account or client secret.
import * as authModule from "./auth.js";
import { rt } from "./rt.js";
import { normalizeChannel } from "./channel.js";
import { TwitchSource } from "./source.js";
import { routes, clearCaches } from "./routes.js";
import { invalidate as invalidateAccess } from "./access.js";
import { clearUserCache } from "./helix.js";

export const platform = "twitch";

export { normalizeChannel, routes };

export function createSource({ channel, emit, ctx }) {
  if (ctx && !rt.ctx) rt.ctx = ctx; // sources can be created before init() in tests
  return new TwitchSource({ channel, emit });
}

export const accounts = {
  state: () => authModule.state(),
  login: () => authModule.login(),
  cancel: () => authModule.cancel(),
  logout: () => authModule.logout(),
};

/** For other modules: ctx.platforms.twitch.getUserToken() -> {token, user, scopes, clientId} | null. */
export const getUserToken = authModule.getUserToken;

let initialised = false;

export function init(ctx) {
  if (initialised) return;
  initialised = true;
  authModule.events.on("change", () => {
    // A different user, or a fresh login: nothing cached for the old one applies.
    invalidateAccess();
    clearUserCache();
    clearCaches();
  });
  authModule.init(ctx);
}
