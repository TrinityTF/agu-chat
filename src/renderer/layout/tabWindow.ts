// Which tabs the tab bar shows when they don't all fit. The bar never scrolls: it shows a run of whole tabs
// that always includes the selected one, and keeps the run where it was, so tabs don't slide around.
// A tab selected past the right end pushes the run right (the leftmost tab drops out); one selected before
// it starts the run there. Leftover room after a close pulls hidden tabs back in from the left.

/** Inclusive range of tab indexes to show. `widths` are the tabs' widths, `room` the space for them. */
export function tabWindow(widths: number[], room: number, active: number, prevStart: number): { start: number; end: number } {
  const n = widths.length;
  if (!n) return { start: 0, end: -1 };
  const a = Math.max(0, Math.min(active, n - 1));
  let start = Math.max(0, Math.min(prevStart, a));
  let used = 0;
  for (let i = start; i <= a; i++) used += widths[i];
  while (used > room && start < a) used -= widths[start++];
  let end = a;
  while (end + 1 < n && used + widths[end + 1] <= room) used += widths[++end];
  while (start > 0 && used + widths[start - 1] <= room) used += widths[--start];
  return { start, end };
}
