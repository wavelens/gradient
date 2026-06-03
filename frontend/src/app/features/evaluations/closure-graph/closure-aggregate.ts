/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { ClosureGraph, ClosureEdge } from '@core/services/evaluations.service';

export const OTHERS_ID = '__others__';

export interface AggNode {
  id: string;
  name: string;
  path: string;
  nar_size: number | null;
  bucketedCount?: number;
}

export interface AggregatedClosure {
  nodes: AggNode[];
  edges: ClosureEdge[];
  total_size_bytes: number | null;
  truncated: boolean;
}

/** Keep the `n` largest nodes by size; collapse the rest into a single
 *  synthetic "others" node sized from the exact total, and reattach edges. */
export function aggregateTopN(graph: ClosureGraph, n: number): AggregatedClosure {
  const sorted = [...graph.nodes].sort((a, b) => (b.nar_size ?? 0) - (a.nar_size ?? 0));
  if (sorted.length <= n) {
    return {
      nodes: sorted,
      edges: graph.edges,
      total_size_bytes: graph.total_size_bytes,
      truncated: graph.truncated,
    };
  }

  const kept = sorted.slice(0, n);
  const dropped = sorted.slice(n);
  const keptIds = new Set(kept.map((node) => node.id));
  const keptSum = kept.reduce((acc, node) => acc + (node.nar_size ?? 0), 0);
  const othersSize = (graph.total_size_bytes ?? keptSum) - keptSum;

  const nodes: AggNode[] = [
    ...kept,
    {
      id: OTHERS_ID,
      name: `others (${dropped.length} packages)`,
      path: '',
      nar_size: othersSize > 0 ? othersSize : 0,
      bucketedCount: dropped.length,
    },
  ];

  const remap = (id: string) => (keptIds.has(id) ? id : OTHERS_ID);
  const seen = new Set<string>();
  const edges: ClosureEdge[] = [];
  for (const e of graph.edges) {
    const source = remap(e.source);
    const target = remap(e.target);
    if (source === target) continue; // collapsed self-loop within others
    const key = `${source}->${target}`;
    if (seen.has(key)) continue;
    seen.add(key);
    edges.push({ source, target });
  }

  return { nodes, edges, total_size_bytes: graph.total_size_bytes, truncated: graph.truncated };
}
