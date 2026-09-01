/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { PALETTE, DARK_ROLES, LIGHT_ROLES, SEMANTIC_ROLES, resolveRole } from './tokens';

describe('design tokens', () => {
  it('documents every role defined in the dark theme', () => {
    const documented = SEMANTIC_ROLES.map((r) => r.name).sort();
    expect(Object.keys(DARK_ROLES).sort()).toEqual(documented);
  });

  it('defines the same roles in both themes', () => {
    expect(Object.keys(LIGHT_ROLES).sort()).toEqual(Object.keys(DARK_ROLES).sort());
  });

  it('maps every role onto an existing palette entry', () => {
    for (const [role, key] of [...Object.entries(DARK_ROLES), ...Object.entries(LIGHT_ROLES)]) {
      expect(PALETTE[key], `${role} points at missing palette key ${key}`).toBeDefined();
    }
  });

  it('never assigns a raw hex to a semantic role', () => {
    for (const key of [...Object.values(DARK_ROLES), ...Object.values(LIGHT_ROLES)]) {
      expect(key.startsWith('--gr-')).toBe(true);
    }
  });

  it('resolves every documented role to a hex in both themes', () => {
    for (const role of SEMANTIC_ROLES) {
      expect(resolveRole(role.name, 'dark')).toMatch(/^#[0-9a-f]{6}$/);
      expect(resolveRole(role.name, 'light')).toMatch(/^#[0-9a-f]{6}$/);
    }
  });
});
