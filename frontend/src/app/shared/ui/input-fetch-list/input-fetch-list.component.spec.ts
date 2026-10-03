/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import type { InputFetch } from '@core/models';
import { InputFetchListComponent } from './input-fetch-list.component';

function render(inputs: InputFetch[]): HTMLElement[] {
  const fixture = TestBed.createComponent(InputFetchListComponent);
  fixture.componentRef.setInput('inputs', inputs);
  fixture.detectChanges();
  return [...fixture.nativeElement.querySelectorAll('.input-row')];
}

const bar = (row: HTMLElement) => row.querySelector('gr-segmented-bar') as HTMLElement;
const segs = (row: HTMLElement) =>
  [...bar(row).querySelectorAll<HTMLElement>('.seg')].map(s => [s.classList[1], s.style.width]);
const size = (row: HTMLElement) => row.querySelector('.input-size')?.textContent?.trim();

describe('InputFetchListComponent', () => {
  it('fills a determinate bar to the downloaded share when the size is known', () => {
    const [row] = render([{ name: 'nixpkgs', state: 'Fetching', downloaded_bytes: 18_400_000, expected_bytes: 46_000_000 }]);
    expect(bar(row).getAttribute('role')).toBe('progressbar');
    expect(bar(row).getAttribute('aria-label')).toBe('nixpkgs');
    expect(bar(row).getAttribute('aria-valuenow')).toBe('40');
    expect(bar(row).getAttribute('aria-valuetext')).toBe('Fetching, 17.5 / 43.9 MiB');
    expect(segs(row)).toEqual([['seg-building', '40%'], ['seg-queued', '60%']]);
    expect(size(row)).toBe('17.5 / 43.9 MiB');
  });

  it('pulses a full bar without a value and shows only the amount when the size is unknown', () => {
    const [row] = render([{ name: 'home-manager', state: 'Fetching', downloaded_bytes: 2_100_000, expected_bytes: 0 }]);
    expect(bar(row).getAttribute('role')).toBe('progressbar');
    expect(bar(row).hasAttribute('aria-valuenow')).toBe(false);
    expect(segs(row)).toEqual([['seg-building', '100%']]);
    expect(size(row)).toBe('2.0 MiB');
  });

  it('shows the queued, done and failed state through the bar tone', () => {
    const rows = render([
      { name: 'crane', state: 'Queued', downloaded_bytes: 0, expected_bytes: 0 },
      { name: 'flake-utils', state: 'Done', downloaded_bytes: 15_300, expected_bytes: 0 },
      { name: 'private-overlay', state: 'Failed', downloaded_bytes: 0, expected_bytes: 0 },
    ]);
    expect(rows.map(r => r.getAttribute('data-state'))).toEqual(['Queued', 'Done', 'Failed']);
    expect(rows.map(segs)).toEqual([[['seg-queued', '100%']], [['seg-completed', '100%']], [['seg-failed', '100%']]]);
    expect(rows.map(size)).toEqual(['', '14.9 KiB', '']);
    expect(rows.map(r => bar(r).getAttribute('aria-valuetext'))).toEqual(['Queued', 'Done, 14.9 KiB', 'Failed']);
  });
});
