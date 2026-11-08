/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { RelativeTimePipe } from './relative-time.pipe';

describe('RelativeTimePipe', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-09-01T12:00:00Z'));
  });
  afterEach(() => vi.useRealTimers());

  const pipe = () => TestBed.runInInjectionContext(() => new RelativeTimePipe());

  it('keeps the same text between two ticks of the shared clock', () => {
    const ago = pipe();
    expect(ago.transform('2026-09-01T11:59:40Z')).toBe('20s ago');
    vi.setSystemTime(new Date('2026-09-01T12:00:00.900Z'));
    expect(ago.transform('2026-09-01T11:59:40Z')).toBe('20s ago');
  });

  it('moves on with the next tick', () => {
    const ago = pipe();
    vi.advanceTimersByTime(5000);
    expect(ago.transform('2026-09-01T11:59:40Z')).toBe('25s ago');
  });
});
