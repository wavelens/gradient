/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { buildGradientCliInstallNetrcCommand, buildNetrcEntryForServerUrl, buildNixConfCacheSubstituterAndKeyLines } from './nix-cache-client-setup';

describe('nix cache client setup', () => {
  it('keys the netrc entry by host only, like curl and nix', () => {
    expect(buildNetrcEntryForServerUrl('https://ci.example.com:8443/base', 'GRAD123')).toBe('machine ci.example.com login gradient password GRAD123');
  });

  it('fills the key into the install-netrc command', () => {
    expect(buildGradientCliInstallNetrcCommand('https://ci.example.com', 'team-cache', 'GRAD123'))
      .toBe('nix run nixpkgs#gradient-cli -- cache install-netrc --server https://ci.example.com --token GRAD123 --cache team-cache');
  });

  it('lists the substituter and its keys for nix.conf', () => {
    expect(buildNixConfCacheSubstituterAndKeyLines('https://ci.example.com/cache/c', ['c:abc']))
      .toBe('extra-substituters = https://ci.example.com/cache/c\nextra-trusted-public-keys = c:abc');
    expect(buildNixConfCacheSubstituterAndKeyLines('https://ci.example.com/cache/c', [])).toContain('<unavailable>');
  });
});
