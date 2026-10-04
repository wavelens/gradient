/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export interface DependencyEdge {
  source: string;
  target: string;
}

/// Longest distance from the root, in topological order: every node and edge is visited once.
export function longestPathDepths(rootId: string, edges: DependencyEdge[]): Map<string, number> {
  const deps = dependenciesOf(edges);
  const reachable = reachableFrom(rootId, deps);
  const pendingDependents = new Map<string, number>();
  for (const id of reachable) {
    for (const dependency of deps.get(id) ?? []) {
      pendingDependents.set(dependency, (pendingDependents.get(dependency) ?? 0) + 1);
    }
  }

  const depths = new Map([[rootId, 0]]);
  const ready = [rootId];
  for (let i = 0; i < ready.length; i++) {
    const id = ready[i];
    const next = depths.get(id)! + 1;
    for (const dependency of deps.get(id) ?? []) {
      depths.set(dependency, Math.max(depths.get(dependency) ?? 0, next));
      const left = pendingDependents.get(dependency)! - 1;
      pendingDependents.set(dependency, left);
      if (left === 0) ready.push(dependency);
    }
  }
  return depths;
}

function dependenciesOf(edges: DependencyEdge[]): Map<string, string[]> {
  const deps = new Map<string, string[]>();
  for (const { source, target } of edges) {
    const list = deps.get(target);
    if (list) list.push(source);
    else deps.set(target, [source]);
  }
  return deps;
}

function reachableFrom(rootId: string, deps: Map<string, string[]>): Set<string> {
  const seen = new Set([rootId]);
  const stack = [rootId];
  while (stack.length) {
    for (const dependency of deps.get(stack.pop()!) ?? []) {
      if (!seen.has(dependency)) {
        seen.add(dependency);
        stack.push(dependency);
      }
    }
  }
  return seen;
}
