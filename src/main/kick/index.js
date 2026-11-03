// STUB: replaced by the kick implementation. See docs/ARCHITECTURE.md ("Platform module").
export const platform = "kick";

export function normalizeChannel(input) {
  const v = String(input || "").trim().toLowerCase();
  if (!v) throw new Error("Enter a channel.");
  return v;
}

export function createSource({ emit }) {
  const conn = { type: "conn", state: "unavailable", detail: "kick is not built yet." };
  return {
    start: () => emit(conn),
    stop: () => {},
    snapshot: () => [conn],
  };
}
