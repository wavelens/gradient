/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { buildClosureSankey, othersIdFor } from './closure-aggregate';
import type { ClosureGraph } from '@core/services/evaluations.service';

function graph(
  nodes: [string, number][],
  edges: [string, string][],
  roots: string[],
): ClosureGraph {
  const total = nodes.reduce((acc, [, s]) => acc + s, 0);
  return {
    roots,
    total_size_bytes: total,
    node_count: nodes.length,
    edge_count: edges.length,
    truncated: false,
    nodes: nodes.map(([id, s]) => ({ id, name: id, path: `/nix/store/${id}`, nar_size: s })),
    edges: edges.map(([source, target]) => ({ source, target })),
  };
}

describe('buildClosureSankey', () => {
  it('accumulates subtree size so each node carries its closure size', () => {
    // d->b->a, c->a ; a is the root.
    const g = graph(
      [['a', 100], ['b', 50], ['c', 10], ['d', 5]],
      [['b', 'a'], ['c', 'a'], ['d', 'b']],
      ['a'],
    );
    const s = buildClosureSankey(g, 30);
    const byId = new Map(s.nodes.map((n) => [n.id, n]));
    expect(byId.get('d')!.value).toBe(5);
    expect(byId.get('b')!.value).toBe(55); // 50 + 5
    expect(byId.get('c')!.value).toBe(10);
    expect(byId.get('a')!.value).toBe(165); // 100 + 55 + 10 == total
    expect(byId.get('a')!.ownSize).toBe(100);
  });

  it('links point dependency -> consumer and carry the dependency subtree size', () => {
    const g = graph(
      [['a', 100], ['b', 50], ['d', 5]],
      [['b', 'a'], ['d', 'b']],
      ['a'],
    );
    const s = buildClosureSankey(g, 30);
    expect(s.links.find((l) => l.source === 'd' && l.target === 'b')!.value).toBe(5);
    expect(s.links.find((l) => l.source === 'b' && l.target === 'a')!.value).toBe(55);
  });

  it('tree-ifies a shared dependency onto a single parent (BFS from root)', () => {
    // s is depended on by both a and b; it must attach to exactly one.
    const g = graph(
      [['r', 0], ['a', 10], ['b', 10], ['s', 5]],
      [['a', 'r'], ['b', 'r'], ['s', 'a'], ['s', 'b']],
      ['r'],
    );
    const s = buildClosureSankey(g, 30);
    const sLinks = s.links.filter((l) => l.source === 's');
    expect(sLinks.length).toBe(1);
    expect(sLinks[0].target).toBe('a');
    expect(s.nodes.find((n) => n.id === 's')!.value).toBe(5);
  });

  it('buckets dropped nodes into their nearest kept ancestor', () => {
    const g = graph(
      [['a', 100], ['b', 50], ['c', 10], ['d', 5]],
      [['b', 'a'], ['c', 'a'], ['d', 'b']],
      ['a'],
    );
    const s = buildClosureSankey(g, 2); // keep a, b only
    const ids = s.nodes.map((n) => n.id);
    expect(ids).not.toContain('c');
    expect(ids).not.toContain('d');

    const othersA = s.nodes.find((n) => n.id === othersIdFor('a'))!;
    expect(othersA.value).toBe(10); // c
    expect(othersA.bucketedCount).toBe(1);

    const othersB = s.nodes.find((n) => n.id === othersIdFor('b'))!;
    expect(othersB.value).toBe(5); // d
    expect(othersB.bucketedCount).toBe(1);

    expect(s.links.find((l) => l.source === othersIdFor('a') && l.target === 'a')).toBeTruthy();
    expect(s.links.find((l) => l.source === othersIdFor('b') && l.target === 'b')).toBeTruthy();
  });

  it('adds no bucket nodes when the whole closure fits within topN', () => {
    const g = graph([['a', 100], ['b', 50]], [['b', 'a']], ['a']);
    const s = buildClosureSankey(g, 30);
    expect(s.nodes.some((n) => n.id.startsWith('__others__'))).toBe(false);
    expect(s.nodes.length).toBe(2);
  });

  it('treats nodes unreachable from any root as their own top-level roots', () => {
    // orphan o has no edge to the root (e.g. its consumer was truncated server-side).
    const g = graph([['a', 100], ['o', 7]], [], ['a']);
    const s = buildClosureSankey(g, 30);
    expect(s.nodes.find((n) => n.id === 'o')!.value).toBe(7);
    expect(s.links.length).toBe(0);
  });
});
