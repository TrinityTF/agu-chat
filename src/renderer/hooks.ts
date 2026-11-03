// Small shared hooks over the bridge. Used by both layout/ and chat/.
import { useEffect, useState } from "react";
import { agu } from "./bridge";
import type { AccountsState, Settings } from "./protocol";

export function useSettings(): Settings | null {
  const [s, setS] = useState<Settings | null>(null);
  useEffect(() => {
    let alive = true;
    agu.settings.get().then((v) => alive && setS(v));
    const off = agu.settings.onChange(setS);
    return () => {
      alive = false;
      off();
    };
  }, []);
  return s;
}

export function useAccounts(): AccountsState | null {
  const [s, setS] = useState<AccountsState | null>(null);
  useEffect(() => {
    let alive = true;
    agu.accounts.get().then((v) => alive && setS(v));
    const off = agu.accounts.onChange(setS);
    return () => {
      alive = false;
      off();
    };
  }, []);
  return s;
}
