/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { vi } from 'vitest';
import type { YearlyRecapResponse } from '@core/models';
import { YearlyRecapAutoOpenService } from '@core/yearly-instance-summary/yearly-recap-auto-open.service';
import { DashboardYearlyRecapCardComponent } from './dashboard-yearly-recap-card.component';

function renderYearlyRecapCardWithStubbedRecapService(recap: YearlyRecapResponse | null) {
  const stub = { recap: signal(recap), reopenRecapSlideShowFromDashboardCard: vi.fn() };
  TestBed.configureTestingModule({
    imports: [DashboardYearlyRecapCardComponent],
    providers: [{ provide: YearlyRecapAutoOpenService, useValue: stub }],
  });
  const fixture = TestBed.createComponent(DashboardYearlyRecapCardComponent);
  fixture.detectChanges();
  return { root: fixture.nativeElement as HTMLElement, stub };
}

describe('DashboardYearlyRecapCardComponent', () => {
  it('show the year of the recap and reopen the slide show', () => {
    const { root, stub } = renderYearlyRecapCardWithStubbedRecapService({
      year: 2026,
      created_at: '2026-12-01T00:00:00',
      data_since: '2026-01-01',
      opened: true,
      statistics: {},
    });
    const buttons = root.querySelectorAll<HTMLButtonElement>('button');
    expect(buttons).toHaveLength(1);
    expect(buttons[0].textContent).toContain('Your Gradient Wrapped 2026');
    buttons[0].click();
    expect(stub.reopenRecapSlideShowFromDashboardCard).toHaveBeenCalledOnce();
  });

  it('render nothing without a recap', () => {
    const { root } = renderYearlyRecapCardWithStubbedRecapService(null);
    expect(root.querySelector('button')).toBeNull();
  });
});
