/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { ClosureGraph } from '@core/services/evaluations.service';

const OTHERS_PREFIX = '__others__:';

export const othersIdFor = (parentId: string): string => `${OTHERS_PREFIX}${parentId}`;

// Type aliases (not interfaces) so they satisfy d3-sankey's index-signature constraint.
export type SankeyNode = {
  id: string;
  name: string;
  /** Accumulated closure size of the node's subtree, in bytes. */
  value: number;
  /** The node's own NAR size, in bytes. */
  ownSize: number;
  /** For bucket nodes: how many packages were collapsed here. */
  bucketedCount?: number;
};

export type SankeyLink = {
  source: string;
  target: string;
  value: number;
};

export interface ClosureSankey {
  nodes: SankeyNode[];
  links: SankeyLink[];
  totalSize: number;
}

/**
 * Turn a closure dependency DAG into a flow-conserving Sankey: reduce it to a
 * rooted tree (each node gets a single parent via BFS from the roots), value
 * every edge by the dependency's accumulated subtree size, and collapse all
 * nodes outside the `topN` largest into a per-parent "others" bucket. Edges
 * point dependency -> consumer, so size accumulates toward the roots.
 */
export function buildClosureSankey(graph: ClosureGraph, topN: number): ClosureSankey {
  const ids = new Set(graph.nodes.map((n) => n.id));
  const own = new Map(graph.nodes.map((n) => [n.id, n.nar_size ?? 0]));
  const name = new Map(graph.nodes.map((n) => [n.id, n.name]));

  const depsOf = new Map<string, string[]>();
  for (const e of graph.edges) {
    if (!ids.has(e.source) || !ids.has(e.target)) continue;
    (depsOf.get(e.target) ?? depsOf.set(e.target, []).get(e.target)!).push(e.source);
  }

  const parent = new Map<string, string>();
  const children = new Map<string, string[]>();
  const order: string[] = [];
  const visited = new Set<string>();
  const rootSet = new Set(graph.roots.filter((r) => ids.has(r)));
  const remaining = [...ids];
  const queue = [...rootSet];
  let head = 0;
  let scan = 0;

  for (;;) {
    if (head >= queue.length) {
      // Drained the roots; seed any unreached node (its consumer was truncated
      // server-side) as its own top-level root.
      while (scan < remaining.length && visited.has(remaining[scan])) scan++;
      if (scan >= remaining.length) break;
      queue.push(remaining[scan++]);
    }
    const node = queue[head++];
    if (visited.has(node)) continue;
    visited.add(node);
    order.push(node);
    for (const dep of depsOf.get(node) ?? []) {
      if (visited.has(dep) || parent.has(dep) || rootSet.has(dep)) continue;
      parent.set(dep, node);
      (children.get(node) ?? children.set(node, []).get(node)!).push(dep);
      queue.push(dep);
    }
  }

  const value = new Map<string, number>();
  const count = new Map<string, number>();
  for (const id of ids) {
    value.set(id, own.get(id) ?? 0);
    count.set(id, 1);
  }
  for (let i = order.length - 1; i >= 0; i--) {
    const node = order[i];
    const p = parent.get(node);
    if (p === undefined) continue;
    value.set(p, value.get(p)! + value.get(node)!);
    count.set(p, count.get(p)! + count.get(node)!);
  }

  const ranked = [...ids].sort((a, b) => value.get(b)! - value.get(a)!);
  const kept = new Set(ranked.slice(0, topN));
  for (const r of graph.roots) if (ids.has(r)) kept.add(r);

  const nodes: SankeyNode[] = [];
  const links: SankeyLink[] = [];

  for (const id of kept) {
    nodes.push({ id, name: name.get(id) ?? id, value: value.get(id)!, ownSize: own.get(id) ?? 0 });
    const p = parent.get(id);
    if (p !== undefined) links.push({ source: id, target: p, value: value.get(id)! });

    let bucketValue = 0;
    let bucketCount = 0;
    for (const child of children.get(id) ?? []) {
      if (kept.has(child)) continue;
      bucketValue += value.get(child)!;
      bucketCount += count.get(child)!;
    }
    if (bucketCount > 0) {
      const bucketId = othersIdFor(id);
      nodes.push({
        id: bucketId,
        name: `others (${bucketCount} packages)`,
        value: bucketValue,
        ownSize: bucketValue,
        bucketedCount: bucketCount,
      });
      links.push({ source: bucketId, target: id, value: bucketValue });
    }
  }

  const totalSize = graph.total_size_bytes ?? [...own.values()].reduce((acc, s) => acc + s, 0);
  return { nodes, links, totalSize };
}
