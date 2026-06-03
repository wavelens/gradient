/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { aggregateTopN, OTHERS_ID } from './closure-aggregate';
import type { ClosureGraph } from '@core/services/evaluations.service';

function graph(nodes: [string, number][], edges: [string, string][], total: number): ClosureGraph {
  return {
    roots: [nodes[0][0]],
    total_size_bytes: total,
    node_count: nodes.length,
    edge_count: edges.length,
    truncated: false,
    nodes: nodes.map(([id, s]) => ({ id, name: id, path: `/nix/store/${id}`, nar_size: s })),
    edges: edges.map(([source, target]) => ({ source, target })),
  };
}

describe('aggregateTopN', () => {
  it('keeps top N nodes and buckets the rest into an others node', () => {
    const g = graph(
      [['a', 100], ['b', 50], ['c', 10], ['d', 5]],
      [['b', 'a'], ['c', 'a'], ['d', 'b']],
      165,
    );
    const r = aggregateTopN(g, 2);
    const ids = r.nodes.map((n) => n.id);
    expect(ids).toContain('a');
    expect(ids).toContain('b');
    expect(ids).toContain(OTHERS_ID);
    const others = r.nodes.find((n) => n.id === OTHERS_ID)!;
    expect(others.nar_size).toBe(15); // total 165 - (100 + 50)
    expect(others.bucketedCount).toBe(2);
  });

  it('reattaches edges from dropped nodes to the others node', () => {
    const g = graph(
      [['a', 100], ['b', 50], ['c', 10]],
      [['b', 'a'], ['c', 'b']],
      160,
    );
    const r = aggregateTopN(g, 2);
    // c is dropped -> edge c->b becomes __others__->b
    expect(r.edges.find((e) => e.source === OTHERS_ID && e.target === 'b')).toBeTruthy();
  });

  it('returns the graph unchanged when node count <= n', () => {
    const g = graph([['a', 100], ['b', 50]], [['b', 'a']], 150);
    const r = aggregateTopN(g, 5);
    expect(r.nodes.length).toBe(2);
    expect(r.nodes.some((n) => n.id === OTHERS_ID)).toBe(false);
  });
});
