/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { DividerComponent } from './divider.component';

describe('gr-divider', () => {
  async function render(orientation?: string) {
    const fixture = TestBed.createComponent(DividerComponent);
    if (orientation) fixture.componentRef.setInput('orientation', orientation);
    fixture.detectChanges();
    await fixture.whenStable();
    return fixture.nativeElement as HTMLElement;
  }

  it('defaults to horizontal and exposes it to assistive tech', async () => {
    const root = await render();
    expect(root.getAttribute('role')).toBe('separator');
    expect(root.getAttribute('aria-orientation')).toBe('horizontal');
    expect(root.classList.contains('gr-divider--vertical')).toBe(false);
  });

  it('applies the vertical orientation', async () => {
    const root = await render('vertical');
    expect(root.getAttribute('aria-orientation')).toBe('vertical');
    expect(root.classList.contains('gr-divider--vertical')).toBe(true);
  });
});
