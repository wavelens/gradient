/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { TableComponent } from './table.component';

@Component({
  standalone: true,
  imports: [TableComponent],
  template: `
    <gr-table>
      <thead>
        <tr>
          <th>Name</th>
          <th aria-sort="ascending">
            <button class="gr-th-sort" type="button">Status</button>
          </th>
        </tr>
      </thead>
      <tbody>
        <tr><td>gradient</td><td>Active</td></tr>
      </tbody>
    </gr-table>
  `,
})
class Host {}

describe('gr-table', () => {
  async function render() {
    const fixture = TestBed.createComponent(Host);
    fixture.detectChanges();
    await fixture.whenStable();
    return fixture.nativeElement as HTMLElement;
  }

  it('wraps the projected rows in a real table element', async () => {
    const root = await render();
    expect(root.querySelector('table')).not.toBeNull();
    expect(root.querySelectorAll('th').length).toBe(2);
    expect(root.querySelectorAll('tbody td').length).toBe(2);
  });

  it('scrolls horizontally rather than overflowing the page', async () => {
    expect((await render()).querySelector('.gr-table__scroll')).not.toBeNull();
  });

  it('keeps the header inside the scroll container so it aligns with the body', async () => {
    const root = await render();
    expect(root.querySelector('.gr-table__scroll thead')).not.toBeNull();
  });
  it('states the sorted column for assistive tech', async () => {
    const root = await render();
    const sorted = root.querySelector('th[aria-sort]');
    expect(sorted?.getAttribute('aria-sort')).toBe('ascending');
  });

  it('gives a sort control one recipe, so every table sorts the same way', async () => {
    const root = await render();
    expect(root.querySelector('th .gr-th-sort')).not.toBeNull();
  });
});
