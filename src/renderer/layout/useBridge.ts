// Layout-only hooks over the bridge (settings/accounts hooks live in ../hooks.ts).
import { useCallback, useEffect, useState } from "react";
import { agu } from "../bridge";
import type { Settings, UpdaterState } from "../protocol";

export function useUpdater(): UpdaterState | null {
  const [s, setS] = useState<UpdaterState | null>(null);
  useEffect(() => {
    let alive = true;
    agu.updater.get().then((v) => alive && setS(v)).catch(() => {});
    const off = agu.updater.onChange(setS);
    return () => {
      alive = false;
      off();
    };
  }, []);
  return s;
}

export function useAppInfo(): { version: string; packaged: boolean } | null {
  const [s, setS] = useState<{ version: string; packaged: boolean } | null>(null);
  useEffect(() => {
    let alive = true;
    agu.app.info().then((v) => alive && setS(v)).catch(() => {});
    return () => {
      alive = false;
    };
  }, []);
  return s;
}

/** Settings with a setter that applies a patch live through agu.settings.set. */
export function useLiveSettings(): [Settings | null, (patch: Partial<Settings>) => void] {
  const [s, setS] = useState<Settings | null>(null);
  useEffect(() => {
    let alive = true;
    agu.settings.get().then((v) => alive && setS(v)).catch(() => {});
    const off = agu.settings.onChange(setS);
    return () => {
      alive = false;
      off();
    };
  }, []);
  const set = useCallback((patch: Partial<Settings>) => {
    setS((cur) => (cur ? { ...cur, ...patch } : cur)); // optimistic; main's answer replaces it
    agu.settings.set(patch).then(setS).catch(() => {});
  }, []);
  return [s, set];
}
