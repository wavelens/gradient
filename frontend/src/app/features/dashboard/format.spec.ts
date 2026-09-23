/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { barsThatFit, formatCpuTime } from './format';

describe('dashboard format', () => {
  it('reads cpu time in hours, then in years once it is large', () => {
    expect(formatCpuTime(3_600_000 * 312)).toBe('312 h');
    expect(formatCpuTime(18.4 * 365 * 24 * 3_600_000)).toBe('18.4 y');
  });

  it('fits as many 7px bars as the width allows, within 1-60', () => {
    expect(barsThatFit(210)).toBe(30);
    expect(barsThatFit(3)).toBe(1);
    expect(barsThatFit(10_000)).toBe(60);
  });
});
