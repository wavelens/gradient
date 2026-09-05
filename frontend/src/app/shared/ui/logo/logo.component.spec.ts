/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { LogoComponent } from './logo.component';

describe('gr-logo', () => {
  async function render() {
    const fixture = TestBed.createComponent(LogoComponent);
    fixture.detectChanges();
    await fixture.whenStable();
    return (fixture.nativeElement as HTMLElement).querySelector('.mark') as HTMLElement;
  }

  it('names the product for assistive tech', async () => {
    const mark = await render();
    expect(mark.getAttribute('role')).toBe('img');
    expect(mark.getAttribute('aria-label')).toBe('Gradient');
  });

  it('draws one mark for both themes rather than swapping files', async () => {
    expect((await render()).querySelector('img')).toBeNull();
  });
});
