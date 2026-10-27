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
import { AppHeaderComponent } from './header.component';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { CommandPaletteService } from '../command-palette/command-palette.service';

const logout = vi.fn(() => ({ subscribe: () => undefined }));

function setup(registrationDisabled: boolean, signedIn = false): ComponentFixture<AppHeaderComponent> {
  logout.mockClear();
  TestBed.configureTestingModule({
    imports: [AppHeaderComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      {
        provide: AuthService,
        useValue: {
          user: signal(signedIn ? { name: 'Alice' } : null),
          isAuthenticated: () => signedIn,
          logout,
        },
      },
      { provide: ConfigService, useValue: { registrationDisabled } },
    ],
  });
  const fixture = TestBed.createComponent(AppHeaderComponent);
  fixture.detectChanges();
  return fixture;
}

function registerLink(root: HTMLElement): HTMLAnchorElement | null {
  return (Array.from(root.querySelectorAll('a')) as HTMLAnchorElement[]).find(
    (el) => (el.textContent ?? '').trim().toLowerCase() === 'register',
  ) ?? null;
}

describe('AppHeaderComponent - registration visibility', () => {
  it('renders the Register link when registration is enabled', () => {
    const fixture = setup(false);
    expect(registerLink(fixture.nativeElement)).not.toBeNull();
  });

  it('hides the Register link when registration is disabled', () => {
    const fixture = setup(true);
    expect(registerLink(fixture.nativeElement)).toBeNull();
  });
});

describe('AppHeaderComponent - user menu', () => {
  function openMenu(): { root: HTMLElement; items: HTMLElement[] } {
    const fixture = setup(false, true);
    const root = fixture.nativeElement as HTMLElement;
    (root.querySelector('.user-menu-trigger') as HTMLElement).click();
    fixture.detectChanges();
    return { root, items: Array.from(document.querySelectorAll<HTMLElement>('.gr-menu__item')) };
  }

  it('folds the user name, settings and logout into one dropdown', () => {
    const { root, items } = openMenu();
    expect(root.querySelector('.header-user')?.textContent).toContain('Alice');
    expect(root.querySelector('.header-user a, .header-user .logout-btn')).toBeNull();
    expect(items.map((item) => item.textContent?.trim())).toEqual([
      expect.stringContaining('Settings'),
      expect.stringContaining('Log out'),
    ]);
  });

  it('logs out from the dropdown', () => {
    const { items } = openMenu();
    items.find((item) => item.textContent?.includes('Log out'))!.click();
    expect(logout).toHaveBeenCalled();
  });
});

describe('AppHeaderComponent - search', () => {
  it('opens the command palette from the header search field', () => {
    const root = setup(false, true).nativeElement as HTMLElement;
    (root.querySelector('.search-trigger') as HTMLElement).click();
    expect(TestBed.inject(CommandPaletteService).isOpen()).toBe(true);
  });

  it('shows no search field to a guest', () => {
    expect((setup(false).nativeElement as HTMLElement).querySelector('.search-trigger')).toBeNull();
  });
});
