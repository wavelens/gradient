/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { describe, expect, it } from 'vitest';
import { eventNameToReadableText, groupCatalog } from './event.model';

const catalog = [
  { name: 'build.completed', durable: true },
  { name: 'build.status_changed', durable: false },
  { name: 'evaluation.failed', durable: true },
  { name: 'task.star', durable: true },
  { name: 'proto.client.*', durable: false },
];

describe('eventNameToReadableText', () => {
  it('Event names turn into readable text.', () => {
    expect(eventNameToReadableText('evaluation.completed')).toBe('Evaluation completed');
    expect(eventNameToReadableText('build.failed_transient')).toBe('Build failed, will retry');
    expect(eventNameToReadableText('cache.nar.fetched')).toBe('Cache NAR fetched');
    expect(eventNameToReadableText('build.*')).toBe('build.*');
  });
});

describe('groupCatalog', () => {
  it('groups durable events by their first segment', () => {
    expect(groupCatalog(catalog)).toEqual([
      { group: 'Build', items: [{ value: 'build.completed', label: 'Completed' }] },
      { group: 'Evaluation', items: [{ value: 'evaluation.failed', label: 'Failed' }] },
      { group: 'Task', items: [{ value: 'task.star', label: 'Star' }] },
    ]);
  });

  it('labels a family with an underscore in words', () => {
    expect(groupCatalog([{ name: 'exec_job.completed', durable: true }])).toEqual([
      { group: 'Exec Job', items: [{ value: 'exec_job.completed', label: 'Completed' }] },
    ]);
  });

  it('keeps only the requested families', () => {
    expect(groupCatalog(catalog, ['build', 'evaluation']).map((g) => g.group)).toEqual([
      'Build',
      'Evaluation',
    ]);
  });

  it('keeps a whole-name prefix without matching longer segments', () => {
    const entries = [
      { name: 'cache.subscription.approve', durable: true },
      { name: 'cache.update', durable: true },
      { name: 'login.success', durable: true },
      { name: 'taskforce.created', durable: true },
    ];
    expect(groupCatalog(entries, ['cache.subscription', 'task'])).toEqual([
      { group: 'Cache', items: [{ value: 'cache.subscription.approve', label: 'Subscription approve' }] },
    ]);
  });
});
