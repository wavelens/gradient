/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Subject, of, throwError } from 'rxjs';
import { TestBed } from '@angular/core/testing';
import { NotifyService } from '@core/services/notify.service';
import { firstLoad as createFirstLoad } from './first-load';

const failure = vi.fn();

function firstLoad() {
  TestBed.configureTestingModule({ providers: [{ provide: NotifyService, useValue: { failure } }] });
  return TestBed.runInInjectionContext(() => createFirstLoad());
}

describe('firstLoad', () => {
  beforeEach(() => failure.mockClear());

  it('starts loading', () => {
    expect(firstLoad().loading()).toBe(true);
  });

  it('waits for every tracked request, not just the quickest', () => {
    const load = firstLoad();
    const workers = new Subject<number>();
    const fleet = new Subject<number>();
    workers.pipe(load.track()).subscribe();
    fleet.pipe(load.track()).subscribe();

    workers.complete();
    expect(load.loading()).toBe(true);

    fleet.complete();
    expect(load.loading()).toBe(false);
  });

  it('holds back the first answer until the slower requests settle, so no half-filled page shows', () => {
    const load = firstLoad();
    const wait = new Subject<number>();
    const summary = new Subject<number>();
    wait.pipe(load.track()).subscribe();
    summary.pipe(load.track()).subscribe();
    wait.next(1);
    wait.complete();
    expect(load.received()).toBe(false);

    summary.next(2);
    summary.complete();
    expect(load.received()).toBe(true);
  });

  it('clears on a failed request too, so an error is not hidden by a spinner', () => {
    const load = firstLoad();
    throwError(() => new Error('down'))
      .pipe(load.track())
      .subscribe({ error: () => {} });
    expect(load.loading()).toBe(false);
  });

  it('keeps a first paint where one request failed on screen and names the failure in a toast', () => {
    const load = firstLoad();
    const ok = new Subject<number>();
    ok.pipe(load.track()).subscribe();
    throwError(() => new Error('down')).pipe(load.track()).subscribe();
    ok.next(1);
    ok.complete();
    expect(load.loading()).toBe(false);
    expect(load.received()).toBe(true);
    expect(load.error()).toBeNull();
    expect(failure).toHaveBeenCalledWith("Couldn't load some items", expect.objectContaining({ message: 'down' }));
  });

  it('replaces the page with the failure when no request of the first paint answered', () => {
    const load = firstLoad();
    const slow = new Subject<number>();
    slow.pipe(load.track()).subscribe();
    throwError(() => new Error('down')).pipe(load.track()).subscribe();
    slow.error(new Error('also down'));
    expect(load.received()).toBe(false);
    expect(load.error()).toBeInstanceOf(Error);
    expect(failure).not.toHaveBeenCalled();
  });

  it('ignores a failed poll once the page is on screen, and a later poll that fully answers clears an old failure', () => {
    const load = firstLoad();
    throwError(() => new Error('down')).pipe(load.track()).subscribe();
    const poll = new Subject<number>();
    poll.pipe(load.track()).subscribe();
    poll.next(1);
    poll.complete();
    expect(load.error()).toBeNull();
    throwError(() => new Error('again')).pipe(load.track()).subscribe();
    expect(load.error()).toBeNull();
  });

  it('keeps the failure when one request of a poll answers and another fails again', () => {
    const load = firstLoad();
    throwError(() => new Error('workers down')).pipe(load.track()).subscribe();
    const pending = new Subject<number>();
    pending.pipe(load.track()).subscribe();
    throwError(() => new Error('workers down')).pipe(load.track()).subscribe();
    pending.next(1);
    pending.complete();
    expect((load.error() as Error).message).toBe('workers down');
  });

  it('reports a failed reload the user asked for, without a spinner', () => {
    const load = firstLoad();
    of(1).pipe(load.track()).subscribe();
    load.refresh();
    throwError(() => new Error('window switch failed')).pipe(load.track()).subscribe();
    expect(load.loading()).toBe(false);
    expect((load.error() as Error).message).toBe('window switch failed');
  });

  it('starts over for Try Again', () => {
    const load = firstLoad();
    throwError(() => new Error('down')).pipe(load.track()).subscribe();
    load.restart();
    expect(load.loading()).toBe(true);
    expect(load.error()).toBeNull();
    expect(load.received()).toBe(false);
  });

  it('stays cleared across the poll that follows', () => {
    const load = firstLoad();
    const poll = new Subject<number>();
    poll.pipe(load.track()).subscribe();
    poll.complete();

    const next = new Subject<number>();
    next.pipe(load.track()).subscribe();
    expect(load.loading()).toBe(false);
  });
});
