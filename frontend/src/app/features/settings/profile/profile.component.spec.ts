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
  TestBed.configureTestingModule({
    imports: [ProfileComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: UserService, useValue: { getUserSettings: () => of(settings(opts)) } },
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

describe('ProfileComponent — access gating', () => {
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
