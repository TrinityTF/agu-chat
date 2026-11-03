// The honest product User-Agent the vendored Rumble code sends. Set once by index.js from ctx.userAgent.
// Rumble's own site code used a browser UA here; this app never does (see docs/ARCHITECTURE.md).
let userAgent = "AguChat";

export function setUserAgent(ua) {
  if (ua) userAgent = String(ua);
}

export function getUserAgent() {
  return userAgent;
}
