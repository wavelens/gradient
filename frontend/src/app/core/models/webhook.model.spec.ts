/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { describe, expect, it } from 'vitest';
import { webhooksBase } from './webhook.model';

describe('webhooksBase', () => {
  it('maps each scope to its API prefix', () => {
    expect(webhooksBase({ kind: 'project', name: 'p' })).toBe('projects/p/webhooks');
    expect(webhooksBase({ kind: 'cache', name: 'c' })).toBe('caches/c/webhooks');
    expect(webhooksBase({ kind: 'instance' })).toBe('admin/webhooks');
  });
});
