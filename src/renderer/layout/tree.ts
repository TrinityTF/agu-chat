// Pure operations on the SplitNode tree (see protocol.ts). Nothing here touches React.
import type { Layout, SourceRef, SplitNode, Tab } from "../protocol";

export type Leaf = Extract<SplitNode, { kind: "leaf" }>;
export type Container = Extract<SplitNode, { kind: "row" | "col" }>;
export type Edge = "left" | "right" | "top" | "bottom";

export function uid(): string {
  return Math.random().toString(36).slice(2, 10) + Date.now().toString(36).slice(-3);
}

export function newLeaf(sources: SourceRef[]): Leaf {
  return { kind: "leaf", id: uid(), sources, modMode: false };
}

export function newTab(sources: SourceRef[] | null = null): Tab {
  return { id: uid(), title: "", root: sources && sources.length ? newLeaf(sources) : null };
}

export function leaves(node: SplitNode | null): Leaf[] {
  if (!node) return [];
  if (node.kind === "leaf") return [node];
  return node.children.flatMap(leaves);
}

export function findLeaf(node: SplitNode | null, id: string): Leaf | null {
  return leaves(node).find((l) => l.id === id) ?? null;
}

export function sourceKey(s: SourceRef): string {
  return `${s.platform}:${s.channel}`;
}

/** Short display name of a source: a URL shows its last meaningful part. */
export function shortName(s: SourceRef): string {
  let c = s.channel.trim();
  if (/^https?:\/\//i.test(c) || /^(www\.)?[a-z0-9-]+\.(com|tv|lol)\//i.test(c)) {
    try {
      const u = new URL(/^https?:\/\//i.test(c) ? c : `https://${c}`);
      const v = u.searchParams.get("v");
      const parts = u.pathname.split("/").filter(Boolean);
      c = v || parts[parts.length - 1] || u.hostname;
    } catch {
      /* keep raw */
    }
  }
  return c;
}

/** Every channel in the tab, once each ("EsfandTV" and "@esfandtv" count as one). */
export function autoTitle(tab: Tab): string {
  const seen = new Set<string>();
  const names: string[] = [];
  for (const n of leaves(tab.root).flatMap((l) => l.sources.map(shortName))) {
    const k = n.replace(/^@/, "").toLowerCase();
    if (seen.has(k)) continue;
    seen.add(k);
    names.push(n);
  }
  return names.length ? names.join(", ") : "New tab";
}

/** A tab with an empty title shows the channels it contains: the first, then "…" when there are more (the full list is its tooltip). */
export function tabTitle(tab: Tab, full = false): string {
  const own = tab.title.trim();
  if (own) return own;
  const all = autoTitle(tab);
  if (full) return all;
  const comma = all.indexOf(", ");
  return comma < 0 ? all : `${all.slice(0, comma)}, …`;
}

// ---- normalisation --------------------------------------------------------

function fixSizes(sizes: number[], n: number): number[] {
  let out = sizes.length === n ? sizes.map((s) => (Number.isFinite(s) && s > 0 ? s : 0)) : Array(n).fill(1);
  let sum = out.reduce((a, b) => a + b, 0);
  if (sum <= 0) {
    out = Array(n).fill(1);
    sum = n;
  }
  return out.map((s) => s / sum);
}

/** Collapse single-child containers, merge a container into a same-kind parent, fix sizes. */
export function normalize(node: SplitNode | null): SplitNode | null {
  if (!node) return null;
  if (node.kind === "leaf") return node;
  const kids: SplitNode[] = [];
  const sizes: number[] = [];
  const own = fixSizes(node.sizes, node.children.length);
  node.children.forEach((c, i) => {
    const n = normalize(c);
    if (!n) return;
    if (n.kind === node.kind) {
      n.children.forEach((g, j) => {
        kids.push(g);
        sizes.push(own[i] * n.sizes[j]);
      });
    } else {
      kids.push(n);
      sizes.push(own[i]);
    }
  });
  if (kids.length === 0) return null;
  if (kids.length === 1) return kids[0];
  return { ...node, children: kids, sizes: fixSizes(sizes, kids.length) };
}

// ---- edits ------------------------------------------------------------------

export function mapLeaf(node: SplitNode | null, id: string, fn: (l: Leaf) => Leaf): SplitNode | null {
  if (!node) return null;
  if (node.kind === "leaf") return node.id === id ? fn(node) : node;
  return { ...node, children: node.children.map((c) => mapLeaf(c, id, fn)!) };
}

export function removeLeaf(node: SplitNode | null, id: string): SplitNode | null {
  if (!node) return null;
  if (node.kind === "leaf") return node.id === id ? null : node;
  const kids: SplitNode[] = [];
  const sizes: number[] = [];
  node.children.forEach((c, i) => {
    const r = removeLeaf(c, id);
    if (r) {
      kids.push(r);
      sizes.push(node.sizes[i] ?? 1 / node.children.length);
    }
  });
  if (kids.length === 0) return null;
  return normalize({ ...node, children: kids, sizes });
}

/** Insert `add` next to the node with id `targetId`, on the given edge, taking half of its room. */
export function insertAt(root: SplitNode | null, targetId: string, add: SplitNode, edge: Edge): SplitNode | null {
  if (!root) return add;
  const axis: "row" | "col" = edge === "left" || edge === "right" ? "row" : "col";
  const before = edge === "left" || edge === "top";

  const wrap = (target: SplitNode): SplitNode => ({
    kind: axis,
    id: uid(),
    children: before ? [add, target] : [target, add],
    sizes: [0.5, 0.5],
  });

  const walk = (node: SplitNode): SplitNode => {
    if (node.kind === "leaf") return node.id === targetId ? wrap(node) : node;
    const idx = node.children.findIndex((c) => c.id === targetId);
    if (idx >= 0) {
      if (node.kind === axis) {
        const children = node.children.slice();
        const sizes = node.sizes.slice();
        const half = sizes[idx] / 2;
        sizes[idx] = half;
        children.splice(before ? idx : idx + 1, 0, add);
        sizes.splice(before ? idx : idx + 1, 0, half);
        return { ...node, children, sizes };
      }
      return { ...node, children: node.children.map((c, i) => (i === idx ? wrap(c) : c)) };
    }
    return { ...node, children: node.children.map(walk) };
  };
  return normalize(walk(root));
}

/** Move leaf `srcId` onto the `edge` of leaf `targetId`. */
export function moveLeaf(root: SplitNode | null, srcId: string, targetId: string, edge: Edge): SplitNode | null {
  if (srcId === targetId) return root;
  const src = findLeaf(root, srcId);
  if (!src || !findLeaf(root, targetId)) return root;
  const without = removeLeaf(root, srcId);
  return insertAt(without, targetId, src, edge);
}

export function setSizes(node: SplitNode | null, containerId: string, sizes: number[]): SplitNode | null {
  if (!node || node.kind === "leaf") return node;
  if (node.id === containerId) return { ...node, sizes };
  return { ...node, children: node.children.map((c) => setSizes(c, containerId, sizes)!) };
}

// ---- layout -------------------------------------------------------------------

export function defaultLayout(): Layout {
  const tab = newTab([{ platform: "twitch", channel: "a_g_u__" }]);
  return { tabs: [tab], active: tab.id };
}

const PLATFORM_SET = new Set(["twitch", "kick", "youtube", "rumble", "x"]);

function cleanNode(n: any): SplitNode | null {
  if (!n || typeof n !== "object") return null;
  if (n.kind === "leaf") {
    const sources: SourceRef[] = Array.isArray(n.sources)
      ? n.sources
          .filter((s: any) => s && PLATFORM_SET.has(s.platform) && typeof s.channel === "string" && s.channel)
          .map((s: any) => ({ platform: s.platform, channel: s.channel }))
      : [];
    if (!sources.length) return null;
    return { kind: "leaf", id: typeof n.id === "string" && n.id ? n.id : uid(), sources, modMode: !!n.modMode };
  }
  if (n.kind === "row" || n.kind === "col") {
    const children = (Array.isArray(n.children) ? n.children : []).map(cleanNode).filter(Boolean) as SplitNode[];
    if (!children.length) return null;
    return {
      kind: n.kind,
      id: typeof n.id === "string" && n.id ? n.id : uid(),
      children,
      sizes: Array.isArray(n.sizes) ? n.sizes.slice(0, children.length) : [],
    };
  }
  return null;
}

/** Make whatever was on disk safe to render. Returns the default layout when nothing usable is there. */
export function sanitizeLayout(raw: unknown): Layout {
  const l = raw as Partial<Layout> | null;
  if (!l || !Array.isArray(l.tabs)) return defaultLayout();
  const tabs: Tab[] = l.tabs
    .filter((t) => t && typeof t === "object")
    .map((t) => ({
      id: typeof t.id === "string" && t.id ? t.id : uid(),
      title: typeof t.title === "string" ? t.title : "",
      root: normalize(cleanNode(t.root)),
    }));
  if (!tabs.length) return defaultLayout();
  const active = tabs.some((t) => t.id === l.active) ? (l.active as string) : tabs[0].id;
  return { tabs, active };
}
