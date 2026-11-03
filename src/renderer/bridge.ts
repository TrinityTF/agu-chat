// Typed access to the preload bridge, plus a fetch-shaped relayApi() so code
// ported from the agu.lol chat page keeps its `const r = await relayApi(...); if (r.ok) await r.json()` shape.
import type { AguBridge, SourceRef } from "./protocol";

declare global {
  interface Window {
    agu: AguBridge;
  }
}

export const agu: AguBridge = window.agu;

export type RelayResponse = { ok: boolean; status: number; json(): Promise<any>; text(): Promise<string> };

export async function relayApi(
  source: SourceRef,
  path: string,
  init: { method?: string; body?: unknown; headers?: unknown } = {},
): Promise<RelayResponse> {
  const { status, body } = await agu.api(source, path, { method: init.method, body: init.body });
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => body,
    text: async () => (typeof body === "string" ? body : JSON.stringify(body)),
  };
}
