/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { formatDate } from '@angular/common';
import { TestBed } from '@angular/core/testing';
import { ServerTimestampToLocalDateTimePipe } from './server-date.pipe';

describe('ServerTimestampToLocalDateTimePipe', () => {
  const pipe = () => TestBed.runInInjectionContext(() => new ServerTimestampToLocalDateTimePipe());

  it('Read API Timestamps Without Zone as UTC', () => {
    const utc = new Date(Date.UTC(2026, 8, 1, 11, 0, 0));
    expect(pipe().transform('2026-09-01 11:00:00', 'medium')).toBe(formatDate(utc, 'medium', 'en-US'));
    expect(pipe().transform('2026-09-01T11:00:00Z', 'medium')).toBe(formatDate(utc, 'medium', 'en-US'));
  });

  it('renders nothing without a timestamp', () => {
    expect(pipe().transform(null)).toBe('');
  });
});
