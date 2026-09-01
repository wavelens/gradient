/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ThemeService } from '@core/services/theme.service';
import { LogoComponent } from './logo.component';

describe('gr-logo', () => {
  async function render(theme: 'light' | 'dark') {
    TestBed.configureTestingModule({
      providers: [{ provide: ThemeService, useValue: { resolved: () => theme } }],
    });
    const fixture = TestBed.createComponent(LogoComponent);
    fixture.detectChanges();
    await fixture.whenStable();
    return (fixture.nativeElement as HTMLElement).querySelector('img') as HTMLImageElement;
  }

  it('uses the light mark on a dark background', async () => {
    expect((await render('dark')).getAttribute('src')).toBe('/images/logo.svg');
  });

  it('uses the dark mark on a light background', async () => {
    expect((await render('light')).getAttribute('src')).toBe('/images/logo-black.png');
  });

  it('names the product for assistive tech', async () => {
    expect((await render('dark')).getAttribute('alt')).toBe('Gradient');
  });
});
