// The resolved shortcut table (settings over defaults), provided by App so menus and tooltips show the
// user's bindings instead of hard-coded ones.
import { createContext, useContext } from "react";
import { resolveHotkeys, type HotkeyAction, type HotkeyTable } from "./hotkeys";

export const HotkeysContext = createContext<HotkeyTable>(resolveHotkeys({}));

/** The first binding of an action ("Ctrl+T"), for a menu hint or tooltip; undefined when unbound. */
export function useHotkeyHint(id: HotkeyAction): string | undefined {
  return useContext(HotkeysContext)[id]?.[0];
}
