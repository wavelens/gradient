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
import { RELEASE_NOTES_SOURCE } from '@core/whats-new/whats-new.service';
import { PendingInvitesService } from '@core/services/pending-invites.service';

const logout = vi.fn(() => ({ subscribe: () => undefined }));

function setup(
  registrationDisabled: boolean,
  signedIn = false,
  publicListing = true,
  serverUnreachable = false,
  superuser = false,
): ComponentFixture<AppHeaderComponent> {
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
          user: signal(signedIn ? { name: 'Alice', superuser } : null),
          isAuthenticated: () => signedIn,
          serverUnreachable: () => serverUnreachable,
          logout,
        },
      },
      { provide: ConfigService, useValue: { registrationDisabled, publicListing } },
      { provide: RELEASE_NOTES_SOURCE, useValue: [] },
    ],
  });
  const fixture = TestBed.createComponent(AppHeaderComponent);
  fixture.detectChanges();
  return fixture;
}

function registerLink(root: HTMLElement): HTMLAnchorElement | null {
  return (Array.from(root.querySelectorAll('a')) as HTMLAnchorElement[]).find(
    (el) => (el.textContent ?? '').trim().toLowerCase() === 'sign up',
  ) ?? null;
}

describe('AppHeaderComponent - registration visibility', () => {
  it('renders the Sign Up link when registration is enabled', () => {
    const fixture = setup(false);
    expect(registerLink(fixture.nativeElement)).not.toBeNull();
  });

  it('hides the Sign Up link when registration is disabled', () => {
    const fixture = setup(true);
    expect(registerLink(fixture.nativeElement)).toBeNull();
  });

  it('shows the custom logo of the instance in the header', () => {
    TestBed.overrideProvider(ConfigService, { useValue: { registrationDisabled: false, publicListing: true, logoUrl: '/brand.svg' } });
    const root = setup(false).nativeElement as HTMLElement;
    expect(root.querySelector('.header-logo img')?.getAttribute('src')).toBe('/brand.svg');
  });

  it('offers no sign-in while the server is unreachable, since the session is unknown', () => {
    const root = setup(false, false, true, true).nativeElement as HTMLElement;
    expect(root.querySelectorAll('.user-link').length).toBe(0);
  });
});

describe('AppHeaderComponent - user menu', () => {
  async function openMenu(superuser = false): Promise<{ root: HTMLElement; items: HTMLElement[] }> {
    const fixture = setup(false, true, true, false, superuser);
    const root = fixture.nativeElement as HTMLElement;
    (root.querySelector('.user-menu-trigger') as HTMLElement).click();
    await fixture.whenStable();
    fixture.detectChanges();
    return { root, items: Array.from(document.querySelectorAll<HTMLElement>('.gr-menu__item')) };
  }

  afterEach(() => document.querySelectorAll('.cdk-overlay-container').forEach((el) => el.remove()));

  it('opens the menu on the first click, before the menu code has loaded', async () => {
    const { root } = await openMenu();
    expect(root.querySelector('.user-menu-trigger')?.getAttribute('aria-expanded')).toBe('true');
  });

  it('folds the user name, settings, invites and logout into one dropdown', async () => {
    const { root, items } = await openMenu();
    expect(root.querySelector('.header-user')?.textContent).toContain('Alice');
    expect(root.querySelector('.header-user a, .header-user .logout-btn')).toBeNull();
    expect(items.map((item) => item.textContent?.trim())).toEqual([
      expect.stringContaining('Settings'),
      expect.stringContaining('My Invites'),
      expect.stringContaining('Log out'),
    ]);
  });

  it('names the menu button by its visible text and leaves the invitation count to the dashboard banner', () => {
    TestBed.overrideProvider(PendingInvitesService, { useValue: { count: () => 3 } });
    const trigger = (setup(false, true).nativeElement as HTMLElement).querySelector('.user-menu-trigger') as HTMLElement;
    expect(trigger.hasAttribute('aria-label')).toBe(false);
    expect(trigger.querySelector('gr-badge')).toBeNull();
  });

  it('leads a superuser to the admin pages, which have no other entry point', async () => {
    const { items } = await openMenu(true);
    const admin = items.filter((item) => ['Event notifications', 'GitHub App'].some((label) => item.textContent?.includes(label)));
    expect(admin.map((item) => item.getAttribute('href'))).toEqual(['/admin/webhooks', '/admin/github-app']);
  });

  it('logs out from the dropdown', async () => {
    const { items } = await openMenu();
    items.find((item) => item.textContent?.includes('Log out'))!.click();
    expect(logout).toHaveBeenCalled();
  });
});

describe('AppHeaderComponent - public listing', () => {
  function navLabels(signedIn: boolean, publicListing: boolean): string[] {
    const root = setup(false, signedIn, publicListing).nativeElement as HTMLElement;
    return Array.from(root.querySelectorAll('a'))
      .filter((a) => ['/projects', '/caches'].includes(a.getAttribute('href') ?? ''))
      .map((a) => (a.textContent ?? '').trim());
  }

  it('shows a guest the project and cache lists while public listing is on', () => {
    expect(navLabels(false, true)).toEqual(['Projects', 'Caches']);
  });

  it('hides the project and cache lists from a guest while public listing is off', () => {
    expect(navLabels(false, false)).toEqual([]);
  });

  it('keeps the project and cache lists for a signed-in user while public listing is off', () => {
    expect(navLabels(true, false)).toEqual(['Projects', 'Caches']);
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
