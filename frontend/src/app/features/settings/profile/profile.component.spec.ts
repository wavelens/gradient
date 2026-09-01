/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { of } from 'rxjs';
import { ProfileComponent } from './profile.component';
import { UserService } from '@core/services/user.service';
import { AuthService } from '@core/services/auth.service';
import { ThemeService } from '@core/services/theme.service';

let profileWrites: unknown[] = [];

function settings(opts: { managed: boolean; oidc: boolean }) {
  return {
    username: 'alice',
    name: 'Alice',
    email: 'alice@example.com',
    is_oidc: opts.oidc,
    managed: opts.managed,
  };
}

function setup(opts: { managed: boolean; oidc: boolean }): ComponentFixture<ProfileComponent> {
  profileWrites = [];
  localStorage.clear();
  document.documentElement.removeAttribute('data-theme');
  TestBed.configureTestingModule({
    imports: [ProfileComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      {
        provide: UserService,
        useValue: {
          getUserSettings: () => of(settings(opts)),
          updateUserSettings: (body: unknown) => {
            profileWrites.push(body);
            return of({});
          },
        },
      },
      { provide: AuthService, useValue: { reloadUser: () => undefined } },
    ],
  });
  const fixture = TestBed.createComponent(ProfileComponent);
  fixture.detectChanges();
  return fixture;
}

function findByText(root: HTMLElement, text: string): HTMLElement | null {
  const target = text.toLowerCase();
  return (Array.from(root.querySelectorAll('button')) as HTMLElement[]).find(
    (el) => (el.textContent ?? '').trim().toLowerCase().includes(target),
  ) ?? null;
}

describe('ProfileComponent - access gating', () => {
  it('renders Save Changes enabled for an unmanaged, non-OIDC account', () => {
    const fixture = setup({ managed: false, oidc: false });
    const save = findByText(fixture.nativeElement, 'save changes') as HTMLButtonElement | null;
    expect(save).not.toBeNull();
    expect(save!.disabled).toBe(false);
  });

  it('renders Save Changes present-but-disabled for a state-managed account', () => {
    const fixture = setup({ managed: true, oidc: false });
    const save = findByText(fixture.nativeElement, 'save changes') as HTMLButtonElement | null;
    expect(save).not.toBeNull();
    expect(save!.disabled).toBe(true);
  });

  it('renders Save Changes present-but-disabled for an OIDC account', () => {
    const fixture = setup({ managed: false, oidc: true });
    const save = findByText(fixture.nativeElement, 'save changes') as HTMLButtonElement | null;
    expect(save).not.toBeNull();
    expect(save!.disabled).toBe(true);
  });

  it('disables Delete Account when managed', () => {
    const fixture = setup({ managed: true, oidc: false });
    const del = findByText(fixture.nativeElement, 'delete account') as HTMLButtonElement | null;
    expect(del).not.toBeNull();
    expect(del!.disabled).toBe(true);
  });
});

describe('ProfileComponent - appearance', () => {
  /// ngModel writes the initial value in a microtask, so the checked segment
  /// only appears after the fixture settles.
  async function ready(opts: { managed: boolean; oidc: boolean }): Promise<ComponentFixture<ProfileComponent>> {
    const fixture = setup(opts);
    await fixture.whenStable();
    fixture.detectChanges();
    return fixture;
  }

  function group(fixture: ComponentFixture<ProfileComponent>): HTMLElement {
    return fixture.nativeElement.querySelector('gr-select-button [role=radiogroup]');
  }

  function themeButton(fixture: ComponentFixture<ProfileComponent>, label: string): HTMLButtonElement {
    return (Array.from(group(fixture).querySelectorAll('button')) as HTMLButtonElement[]).find(
      (el) => (el.textContent ?? '').trim() === label,
    )!;
  }

  it('offers system, light and dark, with the active one checked', async () => {
    const fixture = await ready({ managed: false, oidc: false });
    const labels = Array.from(group(fixture).querySelectorAll('button')).map((el) => el.textContent!.trim());
    expect(labels).toEqual(['System', 'Light', 'Dark']);
    expect(themeButton(fixture, 'System').getAttribute('aria-checked')).toBe('true');
  });

  it('applies a chosen theme immediately without writing it to the profile', async () => {
    const fixture = await ready({ managed: false, oidc: false });
    themeButton(fixture, 'Dark').click();
    fixture.detectChanges();
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
    expect(TestBed.inject(ThemeService).preference()).toBe('dark');
    expect(profileWrites).toEqual([]);
  });

  it('keeps the theme editable on an account whose profile is managed', async () => {
    const fixture = await ready({ managed: true, oidc: false });
    expect(themeButton(fixture, 'Light').disabled).toBe(false);
  });

  it('returns to system, dropping the explicit override', async () => {
    const fixture = await ready({ managed: false, oidc: false });
    themeButton(fixture, 'Light').click();
    fixture.detectChanges();
    themeButton(fixture, 'System').click();
    fixture.detectChanges();
    expect(document.documentElement.hasAttribute('data-theme')).toBe(false);
  });
});
