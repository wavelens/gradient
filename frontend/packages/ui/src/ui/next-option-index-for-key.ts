/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export function getNextOptionIndexForKey(key: string, current: number, count: number): number | undefined {
  if (count === 0) return undefined;
  const target = { ArrowLeft: current - 1, ArrowUp: current - 1, ArrowRight: current + 1, ArrowDown: current + 1, Home: 0, End: count - 1 }[key];
  return target === undefined ? undefined : (target + count) % count;
}
