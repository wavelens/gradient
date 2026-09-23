/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { starPath } from './stars.service';

describe('starPath', () => {
  it('addresses a project', () => {
    expect(starPath({ kind: 'project', project: 'infra' })).toBe('user/stars/projects/infra');
  });

  it('addresses a task under its project', () => {
    expect(starPath({ kind: 'task', project: 'infra', task: 'hosts' })).toBe(
      'user/stars/tasks/infra/hosts',
    );
  });

  it('addresses a cache', () => {
    expect(starPath({ kind: 'cache', cache: 'main' })).toBe('user/stars/caches/main');
  });

  it('encodes every segment', () => {
    expect(starPath({ kind: 'task', project: 'a/b', task: 'c d?' })).toBe(
      'user/stars/tasks/a%2Fb/c%20d%3F',
    );
    expect(starPath({ kind: 'project', project: 'x#y' })).toBe('user/stars/projects/x%23y');
    expect(starPath({ kind: 'cache', cache: 'm&n' })).toBe('user/stars/caches/m%26n');
  });
});
