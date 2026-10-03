/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import type { InputFetch } from '@core/models';
import { InputFetchListComponent } from './input-fetch-list.component';

function render(inputs: InputFetch[]): HTMLElement {
  const fixture = TestBed.createComponent(InputFetchListComponent);
  fixture.componentRef.setInput('inputs', inputs);
  fixture.detectChanges();
  return fixture.nativeElement;
}

describe('InputFetchListComponent', () => {
  it('draws a bar only for an input with a known size', () => {
    const el = render([
      { name: 'nixpkgs', state: 'Fetching', downloaded_bytes: 100, expected_bytes: 400 },
      { name: 'flake-utils', state: 'Fetching', downloaded_bytes: 50, expected_bytes: 0 },
    ]);
    const rows = el.querySelectorAll('.input-row');
    expect(rows.length).toBe(2);
    expect(rows[0].querySelector('[role="progressbar"]')).toBeTruthy();
    expect(rows[1].querySelector('[role="progressbar"]')).toBeNull();
    expect(rows[1].textContent).toContain('50 B');
  });

  it('marks queued, done and failed rows', () => {
    const el = render([
      { name: 'a', state: 'Queued', downloaded_bytes: 0, expected_bytes: 0 },
      { name: 'b', state: 'Done', downloaded_bytes: 9, expected_bytes: 0 },
      { name: 'c', state: 'Failed', downloaded_bytes: 0, expected_bytes: 0 },
    ]);
    expect([...el.querySelectorAll('.input-row')].map(r => r.getAttribute('data-state'))).toEqual(['Queued', 'Done', 'Failed']);
  });
});
