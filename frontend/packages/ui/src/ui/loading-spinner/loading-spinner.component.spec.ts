/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { LoadingSpinnerComponent } from './loading-spinner.component';

describe('gr-loading-spinner', () => {
  async function render(inputs: Record<string, unknown> = {}) {
    const fixture = TestBed.createComponent(LoadingSpinnerComponent);
    for (const [k, v] of Object.entries(inputs)) fixture.componentRef.setInput(k, v);
    fixture.detectChanges();
    await fixture.whenStable();
    return fixture.nativeElement as HTMLElement;
  }

  it('shows a custom message', async () => {
    expect((await render({ message: 'Fetching workers' })).textContent).toContain('Fetching workers');
  });

  it.each(['small', 'medium', 'large'])('exposes the %s size to styling', async (size) => {
    const root = await render({ size });
    expect(root.querySelector('.loading-spinner')?.getAttribute('data-size')).toBe(size);
  });
});
