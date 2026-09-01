/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { StatCardComponent } from './stat-card.component';

describe('gr-stat-card', () => {
  beforeEach(() => TestBed.configureTestingModule({ providers: [provideRouter([])] }));

  async function render(inputs: Record<string, unknown>) {
    const fixture = TestBed.createComponent(StatCardComponent);
    for (const [k, v] of Object.entries(inputs)) fixture.componentRef.setInput(k, v);
    fixture.detectChanges();
    await fixture.whenStable();
    return fixture.nativeElement as HTMLElement;
  }

  it('renders value and label', async () => {
    const root = await render({ icon: 'inbox', value: 12, label: 'Queued' });
    expect(root.textContent).toContain('12');
    expect(root.textContent).toContain('Queued');
  });

  it('renders a string value unchanged', async () => {
    expect((await render({ icon: 'inbox', value: 'n/a', label: 'Queued' })).textContent).toContain('n/a');
  });

  it('drops the icon block when no icon is given', async () => {
    const root = await render({ value: 12, label: 'Queued' });
    expect(root.querySelector('.stat-icon')).toBeNull();
    expect(root.textContent).toContain('Queued');
  });

  it('renders the icon ligature', async () => {
    const root = await render({ icon: 'inbox', value: 1, label: 'A' });
    expect(root.querySelector('.material-symbols-outlined')?.textContent).toContain('inbox');
  });
});
