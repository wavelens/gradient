/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export interface NarFilter {
  hash?: string;
  package?: string;
}

const STORE_DIR = '/nix/store/';
const NIX_BASE32 = '[0-9a-df-np-sv-z]';
const HASH_LENGTH = 32;
const STORE_ENTRY = new RegExp(`^(${NIX_BASE32}{${HASH_LENGTH}})-(.+)$`);
const HASH_PREFIX = new RegExp(`^${NIX_BASE32}{6,${HASH_LENGTH}}$`);

/// Store paths and hashes filter by hash; anything else is a package name.
/// A short run of hash characters only counts as a hash when it carries a digit,
/// so names like `glibc` stay names.
export function parseNarSearch(text: string): NarFilter {
  const query = text.trim();
  if (!query) return {};

  const entry = query.startsWith(STORE_DIR) ? query.slice(STORE_DIR.length).split('/')[0] : query;
  const storeEntry = STORE_ENTRY.exec(entry);
  if (storeEntry) return { hash: storeEntry[1], package: storeEntry[2] };

  const lower = entry.toLowerCase();
  if (HASH_PREFIX.test(lower) && (lower.length === HASH_LENGTH || /\d/.test(lower))) return { hash: lower };

  return { package: query };
}

export function narSearchText(filter: NarFilter): string {
  if (filter.hash && filter.package) return `${filter.hash}-${filter.package}`;
  return filter.hash || filter.package || '';
}
