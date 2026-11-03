// Agu Chat window: tab bar, one SplitView per tab, update bar, dialogs, persistence.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { agu } from "../bridge";
import type { Layout, SourceRef, SplitNode, Tab } from "../protocol";
import { NotebookTabs, type TabActions } from "./NotebookTabs";
import { SplitView } from "./SplitView";
import type { Activity } from "../chat/ChatPane";
import type { SplitActions } from "./SplitHeader";
import { ChannelDialog } from "./ChannelDialog";
import { SettingsDialog } from "./SettingsDialog";
import { useUpdater } from "./useBridge";
import { defaultLayout, findLeaf, insertAt, leaves, mapLeaf, moveLeaf, newLeaf, newTab, removeLeaf, sanitizeLayout, setSizes, type Edge } from "./tree";

type Dialog =
  | { kind: "add"; tabId: string }
  | { kind: "change"; tabId: string; leafId: string }
  | { kind: "split"; tabId: string; leafId: string; dir: "right" | "below" }
  | { kind: "settings" };

export function App() {
  const [layout, setLayout] = useState<Layout | null>(null);
  const [focusByTab, setFocusByTab] = useState<Record<string, string>>({});
  const [dialog, setDialog] = useState<Dialog | null>(null);
  const [barDismissed, setBarDismissed] = useState<string | null>(null);
  // Tabs that got live chat while not selected: "message" (a subtle marker) or "highlight" (a strong one).
  const [unread, setUnread] = useState<Record<string, Activity>>({});
  const updater = useUpdater();

  // ---- load + debounced save ------------------------------------------------
  const latest = useRef<Layout | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    let alive = true;
    agu.layout
      .get()
      .then((l) => alive && setLayout(sanitizeLayout(l)))
      .catch(() => alive && setLayout(defaultLayout()));
    return () => {
      alive = false;
    };
  }, []);

  const flush = useCallback(() => {
    if (timer.current) clearTimeout(timer.current);
    timer.current = null;
    if (latest.current) agu.layout.set(latest.current).catch(() => {});
  }, []);

  useEffect(() => {
    if (!layout) return;
    latest.current = layout;
    if (timer.current) clearTimeout(timer.current);
    timer.current = setTimeout(flush, 500);
  }, [layout, flush]);

  useEffect(() => {
    const onHide = () => {
      if (timer.current) flush();
    };
    window.addEventListener("beforeunload", onHide);
    window.addEventListener("pagehide", onHide);
    return () => {
      window.removeEventListener("beforeunload", onHide);
      window.removeEventListener("pagehide", onHide);
    };
  }, [flush]);

  // ---- derived -----------------------------------------------------------------
  const activeTab = layout?.tabs.find((t) => t.id === layout.active) ?? layout?.tabs[0] ?? null;

  const focusedOf = useCallback(
    (tab: Tab): string | null => {
      const ls = leaves(tab.root);
      const want = focusByTab[tab.id];
      return ls.find((l) => l.id === want)?.id ?? ls[0]?.id ?? null;
    },
    [focusByTab],
  );

  // ---- tab activity markers --------------------------------------------------------
  const activeId = activeTab?.id ?? null;
  const activeRef = useRef(activeId);
  activeRef.current = activeId;
  useEffect(() => {
    if (!activeId) return;
    setUnread((u) => {
      if (!(activeId in u)) return u;
      const next = { ...u };
      delete next[activeId];
      return next;
    });
  }, [activeId]);
  const activityFns = useRef(new Map<string, (kind: Activity) => void>()).current;
  const activityFor = (tabId: string) => {
    let fn = activityFns.get(tabId);
    if (!fn) {
      fn = (kind) => {
        if (tabId === activeRef.current) return;
        setUnread((u) => (u[tabId] === kind || u[tabId] === "highlight" ? u : { ...u, [tabId]: kind }));
      };
      activityFns.set(tabId, fn);
    }
    return fn;
  };

  // ---- tab / split operations (all take ids, so they stay stable) ------------------
  const updateTab = useCallback((tabId: string, fn: (t: Tab) => Tab) => {
    setLayout((l) => (l ? { ...l, tabs: l.tabs.map((t) => (t.id === tabId ? fn(t) : t)) } : l));
  }, []);
  const updateRoot = useCallback(
    (tabId: string, fn: (root: SplitNode | null) => SplitNode | null) => updateTab(tabId, (t) => ({ ...t, root: fn(t.root) })),
    [updateTab],
  );
  const focusLeaf = useCallback((tabId: string, leafId: string) => {
    setFocusByTab((m) => (m[tabId] === leafId ? m : { ...m, [tabId]: leafId }));
  }, []);

  const tabActions: TabActions = useMemo(
    () => ({
      select: (id) => setLayout((l) => (l && l.tabs.some((t) => t.id === id) ? { ...l, active: id } : l)),
      add: () =>
        setLayout((l) => {
          if (!l) return l;
          const t = newTab();
          return { tabs: [...l.tabs, t], active: t.id };
        }),
      close: (id) =>
        setLayout((l) => {
          if (!l) return l;
          const idx = l.tabs.findIndex((t) => t.id === id);
          if (idx < 0) return l;
          const tabs = l.tabs.filter((t) => t.id !== id);
          if (!tabs.length) {
            const t = newTab();
            return { tabs: [t], active: t.id };
          }
          const active = l.active === id ? tabs[Math.min(idx, tabs.length - 1)].id : l.active;
          return { tabs, active };
        }),
      closeOthers: (id) => setLayout((l) => (l ? { tabs: l.tabs.filter((t) => t.id === id), active: id } : l)),
      rename: (id, title) => updateTab(id, (t) => ({ ...t, title })),
      reorder: (id, before) =>
        setLayout((l) => {
          if (!l) return l;
          const from = l.tabs.findIndex((t) => t.id === id);
          if (from < 0) return l;
          const to = before > from ? before - 1 : before;
          if (to === from) return l;
          const tabs = l.tabs.slice();
          const [t] = tabs.splice(from, 1);
          tabs.splice(to, 0, t);
          return { ...l, tabs };
        }),
      openSettings: () => setDialog({ kind: "settings" }),
    }),
    [updateTab],
  );

  const splitActionsFor = useCallback(
    (tabId: string): SplitActions => ({
      focus: (leafId) => focusLeaf(tabId, leafId),
      toggleMod: (leafId) => updateRoot(tabId, (r) => mapLeaf(r, leafId, (l) => ({ ...l, modMode: !l.modMode }))),
      change: (leafId) => setDialog({ kind: "change", tabId, leafId }),
      split: (leafId, dir) => setDialog({ kind: "split", tabId, leafId, dir }),
      close: (leafId) => updateRoot(tabId, (r) => removeLeaf(r, leafId)),
      resize: (containerId, sizes) => updateRoot(tabId, (r) => setSizes(r, containerId, sizes)),
      move: (srcId, targetId, edge: Edge) => {
        updateRoot(tabId, (r) => moveLeaf(r, srcId, targetId, edge));
        focusLeaf(tabId, srcId);
      },
      addFirst: () => setDialog({ kind: "add", tabId }),
    }),
    [focusLeaf, updateRoot],
  );

  // Ctrl+W: close the focused split and hand focus to its neighbour.
  const closeFocusedSplit = useCallback(() => {
    if (!activeTab) return;
    const id = focusedOf(activeTab);
    if (!id) return;
    const before = leaves(activeTab.root);
    const idx = before.findIndex((l) => l.id === id);
    const root = removeLeaf(activeTab.root, id);
    const after = leaves(root);
    updateRoot(activeTab.id, () => root);
    const next = after[Math.min(Math.max(idx - 1, 0), after.length - 1)];
    if (next) focusLeaf(activeTab.id, next.id);
  }, [activeTab, focusedOf, updateRoot, focusLeaf]);

  // ---- dialogs ---------------------------------------------------------------------
  const submitChannels = (sources: SourceRef[]) => {
    const d = dialog;
    setDialog(null);
    if (!d || d.kind === "settings") return;
    const leaf = newLeaf(sources);
    if (d.kind === "add") {
      updateRoot(d.tabId, (r) => (r ? insertAt(r, leaves(r)[0].id, leaf, "right") : leaf));
      focusLeaf(d.tabId, leaf.id);
    } else if (d.kind === "change") {
      updateRoot(d.tabId, (r) => mapLeaf(r, d.leafId, (l) => ({ ...l, sources })));
    } else {
      updateRoot(d.tabId, (r) => insertAt(r, d.leafId, leaf, d.dir === "right" ? "right" : "bottom"));
      focusLeaf(d.tabId, leaf.id);
    }
  };

  // ---- keyboard ---------------------------------------------------------------------
  const keyState = useRef({ layout, dialog });
  keyState.current = { layout, dialog };
  const keyOps = useRef({ tabActions, closeFocusedSplit });
  keyOps.current = { tabActions, closeFocusedSplit };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.altKey) return;
      const { layout, dialog } = keyState.current;
      if (!layout) return;
      const { tabActions: ta, closeFocusedSplit: closeSplit } = keyOps.current;
      const k = e.key;
      const stop = () => {
        e.preventDefault();
        e.stopPropagation();
      };

      if (k.toLowerCase() === "p" && !e.shiftKey) {
        if (dialog && dialog.kind !== "settings") return;
        stop();
        setDialog((d) => (d && d.kind === "settings" ? null : { kind: "settings" }));
        return;
      }
      if (dialog) return; // dialogs own the keyboard

      const idx = layout.tabs.findIndex((t) => t.id === layout.active);
      if (k.toLowerCase() === "t" && !e.shiftKey) {
        stop();
        ta.add();
      } else if (k.toLowerCase() === "w") {
        stop();
        if (e.shiftKey) ta.close(layout.active);
        else closeSplit();
      } else if (k === "Tab" || k === "PageDown" || k === "PageUp") {
        stop();
        const dir = k === "PageUp" || (k === "Tab" && e.shiftKey) ? -1 : 1;
        const n = layout.tabs.length;
        ta.select(layout.tabs[(idx + dir + n) % n].id);
      } else if (/^[1-9]$/.test(k) && !e.shiftKey) {
        stop();
        const n = Number(k);
        const target = n === 9 ? layout.tabs[layout.tabs.length - 1] : layout.tabs[n - 1];
        if (target) ta.select(target.id);
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  // ---- render --------------------------------------------------------------------------
  if (!layout || !activeTab) {
    return <div className="flex h-full items-center justify-center text-xs text-[var(--color-faint)]">Loading…</div>;
  }

  // Tabs are drawn in a stable order (by id) so reordering the tab bar never moves a tab's DOM, which would reset chat scroll.
  const stable = layout.tabs.slice().sort((a, b) => (a.id < b.id ? -1 : 1));
  const showBar = updater?.status === "ready" && barDismissed !== updater.version;

  const dialogLeaf =
    dialog && (dialog.kind === "change" || dialog.kind === "split")
      ? findLeaf(layout.tabs.find((t) => t.id === dialog.tabId)?.root ?? null, dialog.leafId)
      : null;

  return (
    <div className="flex h-full flex-col bg-[var(--color-app)]">
      <NotebookTabs tabs={layout.tabs} active={activeTab.id} actions={tabActions} unread={unread} />
      {showBar && updater && (
        <div className="flex h-6 shrink-0 items-center gap-3 border-b border-[var(--color-line)] bg-[var(--color-accent-strong)] px-3 text-[12px] text-white">
          <span className="min-w-0 flex-1 truncate">Agu Chat {updater.version} is ready — Restart to update</span>
          <button type="button" onClick={() => agu.updater.install().catch(() => {})} className="rounded bg-white/20 px-2 text-[11px] font-semibold hover:bg-white/30">
            Restart
          </button>
          <button type="button" onClick={() => setBarDismissed(updater.version)} className="text-[11px] text-white/80 hover:text-white">
            Later
          </button>
        </div>
      )}
      <div className="relative min-h-0 flex-1">
        {stable.map((t) => {
          const isActive = t.id === activeTab.id;
          return (
            <div key={t.id} className={"absolute inset-0 " + (isActive ? "" : "pointer-events-none invisible")} aria-hidden={!isActive} data-tab-panel={t.id}>
              <SplitView root={t.root} focusedId={focusedOf(t)} tabActive={isActive} actions={splitActionsFor(t.id)} onActivity={activityFor(t.id)} />
            </div>
          );
        })}
      </div>

      {dialog?.kind === "settings" && <SettingsDialog onClose={() => setDialog(null)} />}
      {dialog?.kind === "add" && <ChannelDialog title="Select channel" initial={[]} submitLabel="Add channel" onSubmit={submitChannels} onClose={() => setDialog(null)} />}
      {dialog?.kind === "change" && dialogLeaf && (
        <ChannelDialog title="Change channel(s)" initial={dialogLeaf.sources} submitLabel="Apply" onSubmit={submitChannels} onClose={() => setDialog(null)} />
      )}
      {dialog?.kind === "split" && dialogLeaf && (
        <ChannelDialog
          title={dialog.dir === "right" ? "Add split to the right" : "Add split below"}
          initial={[]}
          submitLabel="Add split"
          onSubmit={submitChannels}
          onClose={() => setDialog(null)}
        />
      )}
    </div>
  );
}
