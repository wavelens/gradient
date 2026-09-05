/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Subject, throwError } from 'rxjs';
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

  it('clears on a failed request too, so an error is not hidden by a spinner', () => {
    const load = firstLoad();
    throwError(() => new Error('down'))
      .pipe(load.track())
      .subscribe({ error: () => {} });
    expect(load.loading()).toBe(false);
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
