// Shared handle to the app context. Platform modules must not import "electron"
// (so they run under plain node with a fake ctx), so main hands ctx over in init().
export const rt = { ctx: null };

export const log = (...args) => rt.ctx?.log?.("[twitch]", ...args);
export const userAgent = () => rt.ctx?.userAgent || "AguChat/0 (+https://github.com/TrinityTF/agu-chat)";
