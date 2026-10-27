/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { of, throwError } from 'rxjs';
import { nameAvailability } from './name-availability';

function setup(taken: string[] = [], reserved: string[] = []) {
  const isAvailable = vi.fn((name: string) => of(!taken.includes(name)));
  const names = TestBed.runInInjectionContext(() => nameAvailability(isAvailable, reserved));
  return { names, isAvailable };
}

describe('nameAvailability', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('asks the server once the typing pauses and reports a taken name', () => {
    const { names, isAvailable } = setup(['infra']);
    names.check('inf');
    names.check('infra');
    expect(names.state()).toBe('checking');
    vi.advanceTimersByTime(400);
    expect(isAvailable).toHaveBeenCalledTimes(1);
    expect(names.state()).toBe('taken');
  });

  it('rejects a malformed or reserved name without asking the server', () => {
    const { names, isAvailable } = setup([], ['settings']);
    names.check('Not Valid');
    vi.advanceTimersByTime(400);
    expect(names.state()).toBe('invalid');
    names.check('settings');
    vi.advanceTimersByTime(400);
    expect(names.state()).toBe('reserved');
    expect(names.blocksSubmit()).toBe(true);
    expect(isAvailable).not.toHaveBeenCalled();
  });

  it('keeps checking after a failed lookup and lets the server judge that name', () => {
    const isAvailable = vi.fn((name: string) => (name === 'flaky' ? throwError(() => new Error('down')) : of(true)));
    const names = TestBed.runInInjectionContext(() => nameAvailability(isAvailable));
    names.check('flaky');
    vi.advanceTimersByTime(400);
    expect(names.state()).toBe('idle');
    expect(names.blocksSubmit()).toBe(false);
    names.check('steady');
    vi.advanceTimersByTime(400);
    expect(names.state()).toBe('available');
  });

  it('derives the name from the display name until the user types one', () => {
    const { names } = setup();
    expect(names.suggest('My Team')).toBe('my-team');
    vi.advanceTimersByTime(400);
    expect(names.state()).toBe('available');
    names.markEdited();
    expect(names.suggest('Other Team')).toBeNull();
    names.reset();
    expect(names.state()).toBe('idle');
    expect(names.suggest('Other Team')).toBe('other-team');
  });
});
