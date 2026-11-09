/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { createMissingPermissionLoadStateError, createNoAccessLoadStateError } from './missing-permission';

describe('createMissingPermissionLoadStateError', () => {
  it.each([
    ['manageWebhooks', 'Only the members with Write or Admin on this project can view this.'],
    ['manageProjectSettings', 'Only the admins of this project can view this.'],
    ['viewProject', 'Only the members of this project can view this.'],
  ] as const)('names the lowest built-in role that grants %s to a member', (permission, text) => {
    expect(createMissingPermissionLoadStateError('project', true, permission).message).toContain(text);
  });

  it('names the cache roles for cache permissions', () => {
    expect(createMissingPermissionLoadStateError('cache', true, 'writeStore').message).toContain('Only the members with Write or Admin on this cache');
    expect(createMissingPermissionLoadStateError('cache', true, 'manageCacheWebhooks').message).toContain('Only the admins of this cache');
  });

  it('tells a non-member that membership comes first', () => {
    expect(createMissingPermissionLoadStateError('project', false, 'manageWebhooks').message).toBe(
      'Only the members of this project can view this. Ask an admin of this project for access.',
    );
  });

  it('keeps the admin text when the caller names no permission', () => {
    expect(createMissingPermissionLoadStateError('project').message).toContain('Only the admins of this project');
    expect(createNoAccessLoadStateError('team', 'admins').message).toContain('Only the admins of this team');
  });
});
