// Subscribes every source of a split through the preload bridge and feeds the frames into a ChatStore.
//
// One subtlety worth a comment: the main process replays a source's snapshot INSIDE the subscribe
// handler (relay.js `subscribe` sends the snapshot frames before it returns the subId), so those frames
// reach `onFrame` BEFORE the subscribe() promise resolves and we know the subId. Frames for an unknown
// subId are therefore buffered while one of our subscribes is in flight, then flushed to the right
// source when the promise resolves (and dropped once none is in flight: they belong to another split).
import { useEffect, useRef, useSyncExternalStore } from "react";
import { agu } from "../bridge";
import type { ConnFrame, Frame, SourceRef } from "../protocol";
import { ChatStore, sourceKey, type Snapshot } from "./store";

export function useChatFeed(
  sources: SourceRef[],
  onStatus?: (status: Record<string, ConnFrame>) => void,
): { store: ChatStore; snap: Snapshot } {
  const storeRef = useRef<ChatStore | null>(null);
  if (!storeRef.current) storeRef.current = new ChatStore();
  const store = storeRef.current;

  // `sources` is usually a new array every render of the parent: only its content matters.
  const sig = sources.map((s) => `${s.platform}\u0000${s.channel}`).join("\u0001");
  const sourcesRef = useRef(sources);
  sourcesRef.current = sources;

  useEffect(() => {
    const refs: SourceRef[] = [];
    const seen = new Set<string>();
    for (const r of sourcesRef.current) {
      const sk = sourceKey(r);
      if (seen.has(sk)) continue;
      seen.add(sk);
      refs.push({ platform: r.platform, channel: r.channel });
    }
    store.sync(refs);

    let dead = false;
    let inflight = 0;
    const idToSk = new Map<string, string>();
    const pending = new Map<string, Frame[]>();
    const subIds: string[] = [];

    const off = agu.onFrame((subId, frame) => {
      const sk = idToSk.get(subId);
      if (sk) {
        store.apply(sk, frame);
        return;
      }
      if (inflight > 0) {
        const list = pending.get(subId);
        if (list) list.push(frame);
        else pending.set(subId, [frame]);
      }
    });

    for (const ref of refs) {
      const sk = sourceKey(ref);
      inflight++;
      agu
        .subscribe(ref)
        .then((subId) => {
          inflight--;
          if (dead) {
            agu.unsubscribe(subId).catch(() => {});
          } else {
            idToSk.set(subId, sk);
            subIds.push(subId);
            // A replayed snapshot replaces whatever an earlier subscription left behind.
            store.resetSource(sk);
            for (const f of pending.get(subId) || []) store.apply(sk, f);
          }
          pending.delete(subId);
          if (inflight === 0) pending.clear();
        })
        .catch((err: unknown) => {
          inflight--;
          if (inflight === 0) pending.clear();
          if (dead) return;
          const msg = err instanceof Error ? err.message : String(err);
          store.apply(sk, { type: "conn", state: "error", detail: msg, canSend: false, readOnlyReason: msg } satisfies ConnFrame);
        });
    }

    return () => {
      dead = true;
      off();
      for (const id of subIds) agu.unsubscribe(id).catch(() => {});
    };
  }, [sig, store]);

  useEffect(() => () => store.dispose(), [store]);

  const snap = useSyncExternalStore(store.subscribe, store.getSnapshot);

  // Tell the split header the latest conn frame per source, whenever one changes.
  const lastStatus = useRef<Record<string, ConnFrame>>({});
  useEffect(() => {
    if (!onStatus) return;
    const next: Record<string, ConnFrame> = {};
    for (const [sk, st] of store.sources) if (st.conn) next[sk] = st.conn;
    const prev = lastStatus.current;
    const keys = Object.keys(next);
    const same = keys.length === Object.keys(prev).length && keys.every((k) => prev[k] === next[k]);
    if (same) return;
    lastStatus.current = next;
    onStatus(next);
  }, [snap, store, onStatus]);

  return { store, snap };
}
