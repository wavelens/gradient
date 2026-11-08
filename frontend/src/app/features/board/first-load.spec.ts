/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Subject, of, throwError } from 'rxjs';
import { firstLoad } from './first-load';

describe('firstLoad', () => {
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

  it('keeps a failure of the first paint so the page can show it instead of empty charts', () => {
    const load = firstLoad();
    const ok = new Subject<number>();
    ok.pipe(load.track()).subscribe();
    throwError(() => new Error('down')).pipe(load.track()).subscribe();
    ok.next(1);
    ok.complete();
    expect(load.loading()).toBe(false);
    expect(load.received()).toBe(true);
    expect((load.error() as Error).message).toBe('down');
  });

  it('ignores a failed poll once the page is on screen, and a later answer clears an old failure', () => {
    const load = firstLoad();
    throwError(() => new Error('down')).pipe(load.track()).subscribe();
    const poll = new Subject<number>();
    poll.pipe(load.track()).subscribe();
    poll.next(1);
    expect(load.error()).toBeNull();
    throwError(() => new Error('again')).pipe(load.track()).subscribe();
    expect(load.error()).toBeNull();
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
