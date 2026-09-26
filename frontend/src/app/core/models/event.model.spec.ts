/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { describe, expect, it } from 'vitest';
import { groupCatalog } from './event.model';

const catalog = [
  { name: 'build.completed', durable: true },
  { name: 'build.status_changed', durable: false },
  { name: 'evaluation.failed', durable: true },
  { name: 'task.star', durable: true },
  { name: 'proto.client.*', durable: false },
];

describe('groupCatalog', () => {
  it('groups durable events by their first segment', () => {
    expect(groupCatalog(catalog)).toEqual([
      { group: 'Build', items: [{ value: 'build.completed', label: 'completed' }] },
      { group: 'Evaluation', items: [{ value: 'evaluation.failed', label: 'failed' }] },
      { group: 'Task', items: [{ value: 'task.star', label: 'star' }] },
    ]);
  });

  it('keeps only the requested families', () => {
    expect(groupCatalog(catalog, ['build', 'evaluation']).map((g) => g.group)).toEqual([
      'Build',
      'Evaluation',
    ]);
  });
});
