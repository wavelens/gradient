/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export const BINARY_CACHE_STAT_TILE_VALUE_EXPLANATIONS = {
  packages: 'Nix store paths currently in this cache.',
  uncompressedSize: 'The total uncompressed archive size (NAR size) of every package stored in this cache.',
  compressedSize: 'The total compressed size of all packages in this cache.',
} as const;
