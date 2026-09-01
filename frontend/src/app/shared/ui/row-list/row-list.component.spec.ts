/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { RowListComponent } from './row-list.component';
import { RowComponent } from './row.component';

@Component({
  standalone: true,
  imports: [RowListComponent, RowComponent],
  template: `
    <gr-row-list>
      <gr-row icon="key" [muted]="true">
        Production key
        <span slot="meta">Last used: never</span>
        <button slot="actions">Revoke</button>
      </gr-row>
      <gr-row>Second key</gr-row>
    </gr-row-list>
  `,
})
class Host {}

describe('gr-row-list', () => {
  async function render() {
    const fixture = TestBed.createComponent(Host);
    fixture.detectChanges();
    await fixture.whenStable();
    return (fixture.nativeElement as HTMLElement);
  }

  it('renders one row per gr-row', async () => {
    expect((await render()).querySelectorAll('gr-row').length).toBe(2);
  });

  it('projects name, meta and actions into their slots', async () => {
    const row = (await render()).querySelector('gr-row')!;
    expect(row.querySelector('.row-name')?.textContent).toContain('Production key');
    expect(row.querySelector('.row-meta')?.textContent).toContain('Last used: never');
    expect(row.querySelector('.row-actions')?.textContent).toContain('Revoke');
  });

  it('renders the icon when given and omits it otherwise', async () => {
    const rows = (await render()).querySelectorAll('gr-row');
    expect(rows[0].querySelector('gr-icon')).not.toBeNull();
    expect(rows[1].querySelector('gr-icon')).toBeNull();
  });

  it('marks a muted row', async () => {
    const rows = (await render()).querySelectorAll('gr-row');
    expect(rows[0].querySelector('.row.is-muted')).not.toBeNull();
    expect(rows[1].querySelector('.row.is-muted')).toBeNull();
  });
});
