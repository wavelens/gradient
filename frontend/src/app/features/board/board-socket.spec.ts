/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { Subject } from 'rxjs';
import { vi } from 'vitest';
import { BoardLiveService, type BoardLiveEvent } from '@core/services/board-live.service';
import { subscribeToBoardSocketWithReload } from './board-socket';

describe('subscribeToBoardSocketWithReload', () => {
  let events: Subject<BoardLiveEvent>;
  let report: (connected: boolean) => void;

  beforeEach(() => {
    vi.useFakeTimers();
    events = new Subject();
    TestBed.configureTestingModule({
      providers: [{
        provide: BoardLiveService,
        useValue: {
          connect: (onChange: (connected: boolean) => void) => {
            report = onChange;
            return events;
          },
        },
      }],
    });
  });

  afterEach(() => vi.useRealTimers());

  it('marks a dropped socket and reloads once it reconnects', () => {
    const reload = vi.fn();
    const connected = TestBed.runInInjectionContext(() => subscribeToBoardSocketWithReload(reload));
    expect(connected()).toBeNull();

    report(true);
    expect(reload).not.toHaveBeenCalled();
    report(false);
    expect(connected()).toBe(false);
    report(true);
    expect(connected()).toBe(true);
    expect(reload).toHaveBeenCalledTimes(1);
  });

  it('reloads at most once per burst of events', () => {
    const reload = vi.fn();
    TestBed.runInInjectionContext(() => subscribeToBoardSocketWithReload(reload));
    const event = { event: 'worker.job_dispatched', at: '', content: {} } as BoardLiveEvent;

    events.next(event);
    events.next(event);
    vi.advanceTimersByTime(1500);
    expect(reload).toHaveBeenCalledTimes(1);
  });
});
