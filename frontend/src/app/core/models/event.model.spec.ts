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

  it('uses one verb set for every scope', () => {
    expect(eventNameToReadableText('project.member.add')).toBe('Project member added');
    expect(eventNameToReadableText('cache.member.create')).toBe('Cache member added');
    expect(eventNameToReadableText('team.member.role_change')).toBe('Team member role changed');
    expect(eventNameToReadableText('cache.member.update')).toBe('Cache member role changed');
    expect(eventNameToReadableText('cache.member.delete')).toBe('Cache member removed');
    expect(eventNameToReadableText('team.grant.remove')).toBe('Team access removed');
    expect(eventNameToReadableText('team.request.approve')).toBe('Team request approved');
    expect(eventNameToReadableText('ssh_key.create')).toBe('SSH key created');
  });

  it('calls webhooks event notifications and names system events in plain words', () => {
    expect(eventNameToReadableText('project.webhook.create')).toBe('Project event notification created');
    expect(eventNameToReadableText('webhook.ping')).toBe('Test delivery');
    expect(eventNameToReadableText('auth.deny')).toBe('Access denied');
    expect(eventNameToReadableText('login.success')).toBe('Signed in');
    expect(groupCatalog([{ name: 'webhook.ping', durable: true }])).toEqual([
      { group: 'Event notification', items: [{ value: 'webhook.ping', label: 'Test delivery' }] },
    ]);
  });
});

describe('groupCatalog', () => {
  it('groups durable events by their first segment', () => {
    expect(groupCatalog(catalog)).toEqual([
      { group: 'Build', items: [{ value: 'build.completed', label: 'Completed' }] },
      { group: 'Evaluation', items: [{ value: 'evaluation.failed', label: 'Failed' }] },
      { group: 'Task', items: [{ value: 'task.star', label: 'Starred' }] },
    ]);
  });

  it('labels a family with an underscore in words', () => {
    expect(groupCatalog([{ name: 'exec_job.completed', durable: true }])).toEqual([
      { group: 'Exec job', items: [{ value: 'exec_job.completed', label: 'Completed' }] },
    ]);
  });

  it('keeps only the requested families', () => {
    expect(groupCatalog(catalog, ['build', 'evaluation']).map((g) => g.group)).toEqual([
      'Build',
      'Evaluation',
    ]);
  });

  it('hides SSH key events while SSH is off', () => {
    const entries = [
      { name: 'ssh_key.create', durable: true },
      { name: 'login.success', durable: true },
    ];
    expect(groupCatalog(entries, undefined, false).map((g) => g.group)).toEqual(['Account']);
    expect(groupCatalog(entries).map((g) => g.group)).toEqual(['SSH key', 'Account']);
  });

  it('merges sign-in, sign-out, registration and session events into one Account group', () => {
    const entries = ['login.success', 'logout', 'register', 'session.revoke', 'build.failed'].map((name) => ({ name, durable: true }));
    expect(groupCatalog(entries)).toEqual([
      {
        group: 'Account',
        items: [
          { value: 'login.success', label: 'Signed in' },
          { value: 'logout', label: 'Signed out' },
          { value: 'register', label: 'Registered' },
          { value: 'session.revoke', label: 'Session revoked' },
        ],
      },
      { group: 'Build', items: [{ value: 'build.failed', label: 'Failed' }] },
    ]);
  });

  it('drops the group name from item labels inside the group', () => {
    const entries = ['exec_job.phase', 'cli.device.start'].map((name) => ({ name, durable: true }));
    expect(groupCatalog(entries).flatMap((group) => group.items.map((item) => `${group.group}: ${item.label}`))).toEqual([
      'Exec job: Changed phase',
      'CLI: Started sign-in',
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
      { group: 'Cache', items: [{ value: 'cache.subscription.approve', label: 'Subscription approved' }] },
    ]);
  });
});
