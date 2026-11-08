/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { of } from 'rxjs';
import { LoginComponent } from './login.component';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';

const EXAMPLE = { id: 'example', label: 'Example' };

function setup(
  config: { loginProviders: { id: string; label: string }[]; passwordLoginEnabled: boolean },
  query: Record<string, string> = {},
): ComponentFixture<LoginComponent> {
  TestBed.configureTestingModule({
    imports: [LoginComponent],
    providers: [
      provideRouter([]),
      { provide: AuthService, useValue: { initialized$: of(true), isAuthenticated: () => false } },
      {
        provide: ConfigService,
        useValue: { oidcEnabled: false, registrationDisabled: true, logoUrl: null, ...config },
      },
      { provide: ActivatedRoute, useValue: { snapshot: { queryParamMap: convertToParamMap(query) } } },
    ],
  });
  const fixture = TestBed.createComponent(LoginComponent);
  fixture.detectChanges();
  return fixture;
}

describe('LoginComponent', () => {
  it('shows one button per login provider', () => {
    const fixture = setup({ loginProviders: [EXAMPLE], passwordLoginEnabled: true });
    expect(fixture.nativeElement.textContent).toContain('Sign in with Example');
  });

  it('shows no provider button without login providers', () => {
    const fixture = setup({ loginProviders: [], passwordLoginEnabled: true });
    expect(fixture.nativeElement.textContent).not.toContain('Sign in with Example');
  });

  it('explains a refused provider sign-in', () => {
    const fixture = setup({ loginProviders: [EXAMPLE], passwordLoginEnabled: true }, { login_error: 'username_taken' });
    expect(fixture.nativeElement.textContent).toContain('The username is taken');
  });

  it('explains that an expired session brought the user here', () => {
    const fixture = setup({ loginProviders: [], passwordLoginEnabled: true }, { reason: 'expired' });
    expect(fixture.nativeElement.textContent).toContain('Session expired');
  });

  it('confirms a new account and fills in its username', () => {
    const fixture = setup({ loginProviders: [], passwordLoginEnabled: true }, { registered: 'dora' });
    expect(fixture.nativeElement.textContent).toContain('Account created');
    expect(fixture.componentInstance.loginForm.value.username).toBe('dora');
  });

  it('falls back to a generic message for unknown errors', () => {
    const fixture = setup({ loginProviders: [EXAMPLE], passwordLoginEnabled: true }, { login_error: 'whatever' });
    expect(fixture.nativeElement.textContent).toContain('Sign-in failed');
  });

  it('says the username is required once the field is left empty', () => {
    const fixture = setup({ loginProviders: [], passwordLoginEnabled: true });
    const root = fixture.nativeElement as HTMLElement;
    root.querySelector('#username')!.dispatchEvent(new Event('blur'));
    fixture.detectChanges();
    expect(root.textContent).toContain('Username or email is required');
    expect(root.textContent).not.toContain('Password is required');
  });

  it('shows the missing fields on Sign In and sends nothing', () => {
    const fixture = setup({ loginProviders: [], passwordLoginEnabled: true });
    const root = fixture.nativeElement as HTMLElement;
    const login = vi.fn();
    (TestBed.inject(AuthService) as unknown as { login: typeof login }).login = login;
    const submit = root.querySelector<HTMLButtonElement>('button[type=submit]')!;
    expect(submit.disabled).toBe(false);
    submit.click();
    fixture.detectChanges();
    expect(root.textContent).toContain('Username or email is required');
    expect(root.textContent).toContain('Password is required');
    expect(login).not.toHaveBeenCalled();
  });

  it('hides the password form when password login is off', () => {
    const fixture = setup({ loginProviders: [EXAMPLE], passwordLoginEnabled: false });
    expect(fixture.nativeElement.querySelector('form')).toBeNull();
  });
});
