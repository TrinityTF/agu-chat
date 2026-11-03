// STUB: replaced by the youtube implementation. See docs/ARCHITECTURE.md ("Platform module").
export const platform = "youtube";

export function normalizeChannel(input) {
  const v = String(input || "").trim().toLowerCase();
  if (!v) throw new Error("Enter a channel.");
  return v;
}

export function createSource({ emit }) {
  const conn = { type: "conn", state: "unavailable", detail: "youtube is not built yet." };
  return {
    start: () => emit(conn),
    stop: () => {},
    snapshot: () => [conn],
  };
}
