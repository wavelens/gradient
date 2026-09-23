/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { signal } from '@angular/core';
import { HeaderComponent } from './header.component';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { CommandPaletteService } from '../command-palette/command-palette.service';

function setup(registrationDisabled: boolean, signedIn = false): ComponentFixture<HeaderComponent> {
  TestBed.configureTestingModule({
    imports: [HeaderComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      {
        provide: AuthService,
        useValue: {
          user: signal(null),
          isAuthenticated: () => signedIn,
          logout: () => ({ subscribe: () => undefined }),
        },
      },
      { provide: ConfigService, useValue: { registrationDisabled } },
    ],
  });
  const fixture = TestBed.createComponent(HeaderComponent);
  fixture.detectChanges();
  return fixture;
}

function registerLink(root: HTMLElement): HTMLAnchorElement | null {
  return (Array.from(root.querySelectorAll('a')) as HTMLAnchorElement[]).find(
    (el) => (el.textContent ?? '').trim().toLowerCase() === 'register',
  ) ?? null;
}

describe('HeaderComponent - registration visibility', () => {
  it('renders the Register link when registration is enabled', () => {
    const fixture = setup(false);
    expect(registerLink(fixture.nativeElement)).not.toBeNull();
  });

  it('hides the Register link when registration is disabled', () => {
    const fixture = setup(true);
    expect(registerLink(fixture.nativeElement)).toBeNull();
  });
});

describe('HeaderComponent - search', () => {
  it('opens the command palette from the header search field', () => {
    const root = setup(false, true).nativeElement as HTMLElement;
    (root.querySelector('.search-trigger') as HTMLElement).click();
    expect(TestBed.inject(CommandPaletteService).isOpen()).toBe(true);
  });

  it('shows no search field to a guest', () => {
    expect((setup(false).nativeElement as HTMLElement).querySelector('.search-trigger')).toBeNull();
  });
});
