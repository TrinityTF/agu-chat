// The app-wide singletons: one IRC connection and one EventSub session for every Twitch source.
import { IrcHub } from "./ircHub.js";
import { EventSubManager } from "./eventsub.js";

export const hub = new IrcHub();
export const esub = new EventSubManager();
