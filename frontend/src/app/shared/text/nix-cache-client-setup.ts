/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export interface CacheClientSetupTexts {
  netrcCredentialsLine: string;
  nixConfExtraSubstitutersAndTrustedPublicKeysLines: string;
  installNetrcCommand: string;
}

export function buildNixConfCacheSubstituterAndKeyLines(cacheUrl: string, publicKeys: readonly string[]): string {
  return `extra-substituters = ${cacheUrl}\nextra-trusted-public-keys = ${publicKeys.length ? publicKeys.join(' ') : '<unavailable>'}`;
}

export function buildGradientCliInstallNetrcCommand(serverUrl: string, cache: string, token: string): string {
  return `nix run nixpkgs#gradient-cli -- cache install-netrc --server ${serverUrl} --token ${token} --cache ${cache}`;
}

export function buildNetrcEntryForServerUrl(serverUrl: string, token: string): string {
  const host = serverUrl.replace(/^[a-z]+:\/\//i, '').split(/[/:]/)[0];
  return `machine ${host} login gradient password ${token}`;
}
