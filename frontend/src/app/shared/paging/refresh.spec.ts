/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { of, throwError } from 'rxjs';
import { runReloadIntervalWhileTabVisible } from './refresh';

describe('runReloadIntervalWhileTabVisible', () => {
  let visibility: DocumentVisibilityState;

  beforeEach(() => {
    vi.useFakeTimers();
    visibility = 'visible';
    vi.spyOn(document, 'visibilityState', 'get').mockImplementation(() => visibility);
  });

  afterEach(() => {
    TestBed.resetTestingModule();
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('reloads on every tick while the tab is visible and keeps going after a failure', () => {
    const fetch = vi.fn().mockReturnValueOnce(throwError(() => new Error('down'))).mockReturnValue(of(1));
    TestBed.runInInjectionContext(() => runReloadIntervalWhileTabVisible(fetch, 1000));
    vi.advanceTimersByTime(3000);
    expect(fetch).toHaveBeenCalledTimes(3);
  });

  it('skips ticks in a hidden tab and catches up when the tab shows again', () => {
    const fetch = vi.fn(() => of(1));
    TestBed.runInInjectionContext(() => runReloadIntervalWhileTabVisible(fetch, 1000));
    visibility = 'hidden';
    vi.advanceTimersByTime(3000);
    expect(fetch).not.toHaveBeenCalled();

    visibility = 'visible';
    document.dispatchEvent(new Event('visibilitychange'));
    expect(fetch).toHaveBeenCalledTimes(1);
  });
});
