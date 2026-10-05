/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { SkeletonComponent } from './skeleton.component';

describe('gr-skeleton', () => {
  async function render(inputs: Record<string, unknown> = {}) {
    const fixture = TestBed.createComponent(SkeletonComponent);
    for (const [k, v] of Object.entries(inputs)) fixture.componentRef.setInput(k, v);
    fixture.detectChanges();
    await fixture.whenStable();
    return fixture.nativeElement as HTMLElement;
  }

  it('stays out of the accessibility tree', async () => {
    expect((await render()).getAttribute('aria-hidden')).toBe('true');
  });

  it('takes the size of the content it stands in for', async () => {
    const root = await render({ width: '12ch', height: '2rem' });
    expect(root.style.width).toBe('12ch');
    expect(root.style.height).toBe('2rem');
  });
});
