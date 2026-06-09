/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { normalizeProbeUrl, isGradientCacheInfo } from './cache-upstream-probe';

describe('normalizeProbeUrl', () => {
  it('accepts absolute http(s) URLs and strips trailing slashes', () => {
    expect(normalizeProbeUrl('https://cache.nixos.org')).toBe('https://cache.nixos.org');
    expect(normalizeProbeUrl('  http://cache.example.com/  ')).toBe('http://cache.example.com');
  });

  it('rejects scheme-less or relative input that would resolve to our own origin', () => {
    expect(normalizeProbeUrl('randomtext')).toBeNull();
    expect(normalizeProbeUrl('cache.nixos.org')).toBeNull();
    expect(normalizeProbeUrl('/randomtext')).toBeNull();
    expect(normalizeProbeUrl('')).toBeNull();
    expect(normalizeProbeUrl('   ')).toBeNull();
  });

  it('rejects non-http protocols', () => {
    expect(normalizeProbeUrl('ftp://cache.example.com')).toBeNull();
    expect(normalizeProbeUrl('file:///etc/passwd')).toBeNull();
  });
});

describe('isGradientCacheInfo', () => {
  it('accepts a body with both Gradient fields', () => {
    expect(isGradientCacheInfo({ GradientVersion: '0.1.0', GradientUrl: 'https://g.example.com' })).toBe(true);
  });

  it('rejects bodies missing the Gradient fields', () => {
    expect(isGradientCacheInfo({})).toBe(false);
    expect(isGradientCacheInfo({ GradientVersion: '0.1.0' })).toBe(false);
    expect(isGradientCacheInfo({ WantMassQuery: true, StoreDir: '/nix/store' })).toBe(false);
  });

  it('rejects non-object bodies', () => {
    expect(isGradientCacheInfo(null)).toBe(false);
    expect(isGradientCacheInfo('<html>index</html>')).toBe(false);
    expect(isGradientCacheInfo(undefined)).toBe(false);
  });
});
