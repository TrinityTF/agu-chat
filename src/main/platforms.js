// Every platform module, keyed by platform name. See docs/ARCHITECTURE.md ("Platform module").
import * as twitch from "./twitch/index.js";
import * as kick from "./kick/index.js";
import * as youtube from "./youtube/index.js";
import * as rumble from "./rumble/index.js";
import * as x from "./x/index.js";

export const modules = { twitch, kick, youtube, rumble, x };
