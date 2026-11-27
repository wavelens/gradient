/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { LogoComponent } from './logo.component';

describe('gr-logo', () => {
  async function renderHost(src: string | null = null) {
    const fixture = TestBed.createComponent(LogoComponent);
    fixture.componentRef.setInput('src', src);
    fixture.detectChanges();
    await fixture.whenStable();
    return fixture.nativeElement as HTMLElement;
  }

  it('shows a configured image in its own colors instead of the mark', async () => {
    const host = await renderHost('https://example.com/logo.png');
    expect(host.querySelector('img')?.getAttribute('src')).toBe('https://example.com/logo.png');
    expect(host.querySelector('.mark')).toBeNull();
  });
});
