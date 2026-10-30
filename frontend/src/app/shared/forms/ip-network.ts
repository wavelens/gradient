/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

const IPV4 = /^(25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)(\.(25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)){3}$/;

function isIpv6(address: string): boolean {
  if (!/^[0-9a-f:]+$/i.test(address) || (address.match(/::/g) ?? []).length > 1) return false;
  const groups = address.split(':').filter((g) => g !== '');
  const valid = groups.every((g) => g.length <= 4);
  return valid && (address.includes('::') ? groups.length < 8 : groups.length === 8);
}

export function isIpNetwork(value: string): boolean {
  const [address, prefix, ...rest] = value.split('/');
  if (rest.length > 0) return false;
  const v4 = IPV4.test(address);
  if (!v4 && !isIpv6(address)) return false;
  if (prefix === undefined) return true;
  const bits = Number(prefix);
  return /^\d+$/.test(prefix) && bits <= (v4 ? 32 : 128);
}

export function splitLines(text: string): string[] {
  return text.split('\n').map((s) => s.trim()).filter((s) => s.length > 0);
}
