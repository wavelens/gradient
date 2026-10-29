/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

function parts(version: string): { release: number[]; prerelease: string } {
  const [core, prerelease = ''] = version.split('+')[0].split(/-(.*)/s);
  return { release: core.split('.').map((n) => Number.parseInt(n, 10) || 0), prerelease };
}

export function releaseLine(version: string): string {
  const [major = 0, minor = 0] = parts(version).release;
  return `${major}.${minor}`;
}

export function compareVersions(a: string, b: string): number {
  const left = parts(a);
  const right = parts(b);
  for (let i = 0; i < Math.max(left.release.length, right.release.length); i++) {
    const diff = (left.release[i] ?? 0) - (right.release[i] ?? 0);
    if (diff !== 0) return Math.sign(diff);
  }
  if (left.prerelease === right.prerelease) return 0;
  if (!left.prerelease) return 1;
  if (!right.prerelease) return -1;
  return Math.sign(left.prerelease.localeCompare(right.prerelease, undefined, { numeric: true }));
}
