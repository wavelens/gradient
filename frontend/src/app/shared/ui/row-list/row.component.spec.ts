/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { RowComponent } from './row.component';

describe('gr-row', () => {
  async function render(inputs: Record<string, unknown> = {}) {
    const fixture = TestBed.createComponent(RowComponent);
    for (const [k, v] of Object.entries(inputs)) fixture.componentRef.setInput(k, v);
    fixture.detectChanges();
    await fixture.whenStable();
    return fixture.nativeElement as HTMLElement;
  }

  it('renders the row scaffold', async () => {
    const root = await render();
    expect(root.querySelector('.row-name')).not.toBeNull();
    expect(root.querySelector('.row-meta')).not.toBeNull();
    expect(root.querySelector('.row-actions')).not.toBeNull();
  });

  it('omits the icon by default', async () => {
    expect((await render()).querySelector('gr-icon')).toBeNull();
  });

  it('renders the icon it is given', async () => {
    expect((await render({ icon: 'key' })).querySelector('gr-icon')).not.toBeNull();
  });

  it('is not muted by default', async () => {
    expect((await render()).querySelector('.row.is-muted')).toBeNull();
  });

  it('marks itself muted on request', async () => {
    expect((await render({ muted: true })).querySelector('.row.is-muted')).not.toBeNull();
  });
});
