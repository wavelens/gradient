/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { Observable, of, throwError } from 'rxjs';
import { StarsService, starPath } from './stars.service';
import { ApiService } from './api.service';
import { AuthService } from './auth.service';
import { UserStars } from '@core/models';

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

describe('StarsService', () => {
  const stars: UserStars = {
    projects: ['acme'],
    tasks: [{ project: 'acme', task: 'hosts' }],
    caches: ['main'],
  };

  function setup(authenticated: boolean, response: Observable<UserStars> = of(stars)) {
    const get = vi.fn(() => response);
    TestBed.configureTestingModule({
      providers: [
        { provide: ApiService, useValue: { get } },
        { provide: AuthService, useValue: { initialized$: of(true), isAuthenticated: () => authenticated } },
      ],
    });
    return { service: TestBed.inject(StarsService), get };
  }

  function first(source: Observable<boolean>): boolean[] {
    const seen: boolean[] = [];
    source.subscribe((v) => seen.push(v));
    return seen;
  }

  it('lists the caller stars', () => {
    const { service, get } = setup(true);
    let listed: UserStars | undefined;
    service.list().subscribe((s) => (listed = s));
    expect(get).toHaveBeenCalledWith('user/stars');
    expect(listed).toEqual(stars);
  });

  it('tells whether a target is starred', () => {
    const { service } = setup(true);
    expect(first(service.starred({ kind: 'project', project: 'acme' }))).toEqual([true]);
    expect(first(service.starred({ kind: 'project', project: 'other' }))).toEqual([false]);
    expect(first(service.starred({ kind: 'task', project: 'acme', task: 'hosts' }))).toEqual([true]);
    expect(first(service.starred({ kind: 'task', project: 'other', task: 'hosts' }))).toEqual([false]);
    expect(first(service.starred({ kind: 'cache', cache: 'main' }))).toEqual([true]);
    expect(first(service.starred({ kind: 'cache', cache: 'acme' }))).toEqual([false]);
  });

  it('asks nothing for a guest', () => {
    const { service, get } = setup(false);
    expect(first(service.starred({ kind: 'cache', cache: 'main' }))).toEqual([false]);
    expect(get).not.toHaveBeenCalled();
  });

  it('reads a failed list as unstarred', () => {
    const { service } = setup(true, throwError(() => new Error('down')));
    expect(first(service.starred({ kind: 'project', project: 'acme' }))).toEqual([false]);
  });
});
