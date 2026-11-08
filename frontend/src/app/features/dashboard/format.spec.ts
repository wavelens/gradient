/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { barsThatFit, formatCpuTime } from './format';

describe('dashboard format', () => {
  it('CPU time values below 1 hour show a duration, then hours, then years.', () => {
    expect(formatCpuTime(0)).toBe('0 ms');
    expect(formatCpuTime(20 * 60_000)).toBe('20m 00s');
    expect(formatCpuTime(3_600_000 * 312)).toBe('312 h');
    expect(formatCpuTime(18.4 * 365 * 24 * 3_600_000)).toBe('18.4 y');
  });

  it('Render short CPU times as hours, minutes and seconds. Show a dash when the worker reports no CPU time.', () => {
    expect(formatCpuTime(725_000)).toBe('12m 05s');
    expect(formatCpuTime(null)).toBe('-');
  });

  it('fits as many 7px bars as the width allows, within 1-60', () => {
    expect(barsThatFit(210)).toBe(30);
    expect(barsThatFit(3)).toBe(1);
    expect(barsThatFit(10_000)).toBe(60);
  });
});
