/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { longestPathDepths } from './dependency-depths';

const edge = (dependency: string, dependent: string) => ({ source: dependency, target: dependent });

describe('longestPathDepths', () => {
  it('places a dependency below its deepest dependent', () => {
    const depths = longestPathDepths('root', [
      edge('a', 'root'),
      edge('b', 'a'),
      edge('b', 'root'),
    ]);
    expect(depths.get('root')).toBe(0);
    expect(depths.get('a')).toBe(1);
    expect(depths.get('b')).toBe(2);
  });

  it('lays out a densely connected closure in linear time', () => {
    const ids = Array.from({ length: 40 }, (_, i) => `n${i}`);
    const edges = ids.flatMap((dependent, i) => ids.slice(i + 1).map((dependency) => edge(dependency, dependent)));
    const depths = longestPathDepths('n0', edges);
    expect(ids.map((id) => depths.get(id))).toEqual(ids.map((_, i) => i));
  });

  it('leaves nodes the root does not reach without a depth', () => {
    const depths = longestPathDepths('root', [edge('a', 'root'), edge('x', 'y')]);
    expect(depths.has('x')).toBe(false);
    expect(depths.has('y')).toBe(false);
  });

  it('terminates on a dependency cycle', () => {
    const depths = longestPathDepths('root', [edge('a', 'root'), edge('b', 'a'), edge('a', 'b')]);
    expect(depths.get('root')).toBe(0);
    expect(depths.has('a')).toBe(true);
  });
});
