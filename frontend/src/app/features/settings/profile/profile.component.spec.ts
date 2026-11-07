/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { of, throwError } from 'rxjs';
import { ApiError } from '@core/services/api.service';
import { ProfileComponent } from './profile.component';
import { UserService } from '@core/services/user.service';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { ThemeService } from '@core/services/theme.service';
import { NotifyService } from '@core/services/notify.service';

let profileWrites: unknown[] = [];
let disconnects: string[] = [];
let notify = { success: vi.fn(), failure: vi.fn() };

interface SetupOptions {
  managed: boolean;
  oidc: boolean;
  ssh?: boolean;
  password?: boolean;
  provider?: { connected: boolean; canDisconnect: boolean };
  query?: Record<string, string>;
  loadFailure?: ApiError;
  writeFailure?: ApiError;
}

function settings(opts: SetupOptions) {
  return {
    username: 'alice',
    name: 'Alice',
    email: 'alice@example.com',
    is_oidc: opts.oidc,
    has_password: opts.password ?? !opts.oidc,
    login_providers: opts.provider
      ? [{ id: 'example', label: 'Example', connected: opts.provider.connected, can_disconnect: opts.provider.canDisconnect }]
      : [],
    managed: opts.managed,
  };
}

function setup(opts: SetupOptions): ComponentFixture<ProfileComponent> {
  profileWrites = [];
  disconnects = [];
  notify = { success: vi.fn(), failure: vi.fn() };
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
          getUserSettings: () => (opts.loadFailure ? throwError(() => opts.loadFailure) : of(settings(opts))),
          updateUserSettings: (body: unknown) => {
            profileWrites.push(body);
            return opts.writeFailure ? throwError(() => opts.writeFailure) : of({});
          },
          disconnectLoginProvider: (id: string) => {
            disconnects.push(id);
            return opts.writeFailure ? throwError(() => opts.writeFailure) : of('Account disconnected');
          },
          deleteUser: () => throwError(() => opts.writeFailure),
        },
      },
      { provide: NotifyService, useValue: notify },
      {
        provide: ActivatedRoute,
        useValue: { snapshot: { queryParamMap: convertToParamMap(opts.query ?? {}) } },
      },
      { provide: AuthService, useValue: { reloadUser: () => undefined } },
      { provide: ConfigService, useValue: { sshEnabled: opts.ssh ?? false } },
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

describe('ProfileComponent - saving the profile', () => {
  it('sends only the changed name, not the unchanged username and email', () => {
    const fixture = setup({ managed: false, oidc: false });
    fixture.componentInstance.formData.name = 'Alice Liddell';
    fixture.componentInstance.saveSettings();
    expect(profileWrites).toEqual([{ name: 'Alice Liddell' }]);
  });

  it('sends nothing when no field changed', () => {
    const fixture = setup({ managed: false, oidc: false });
    fixture.componentInstance.saveSettings();
    expect(profileWrites).toEqual([]);
  });

  it('does not resend a field once it is saved', () => {
    const fixture = setup({ managed: false, oidc: false });
    fixture.componentInstance.formData.username = 'alice2';
    fixture.componentInstance.saveSettings();
    fixture.componentInstance.formData.email = 'alice2@example.com';
    fixture.componentInstance.saveSettings();
    expect(profileWrites).toEqual([{ username: 'alice2' }, { email: 'alice2@example.com' }]);
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

  function sectionTitled(fixture: ComponentFixture<ProfileComponent>, title: string): HTMLElement {
    return (Array.from(fixture.nativeElement.querySelectorAll('gr-settings-section')) as HTMLElement[]).find(
      (el) => (el.querySelector('h2')?.textContent ?? '').trim() === title,
    )!;
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

  it('sits on a row, since one control does not fill a form card', async () => {
    const fixture = await ready({ managed: false, oidc: false });
    const section = sectionTitled(fixture, 'Appearance');
    expect(section.querySelector('.settings-section__body--card')).toBeNull();
    expect(section.querySelector('.row-actions gr-select-button')).not.toBeNull();
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

describe('ProfileComponent - SSH keys row', () => {
  it('is hidden while SSH is disabled', () => {
    const fixture = setup({ managed: false, oidc: false });
    expect(fixture.nativeElement.textContent).not.toContain('SSH Keys');
  });

  it('is shown when SSH is enabled', () => {
    const fixture = setup({ managed: false, oidc: false, ssh: true });
    expect(fixture.nativeElement.textContent).toContain('SSH Keys');
  });
});

describe('ProfileComponent - connected accounts', () => {
  it('hides the section without login providers', () => {
    const fixture = setup({ managed: false, oidc: false });
    expect(fixture.nativeElement.textContent).not.toContain('Connected Accounts');
  });

  it('offers Connect for an unconnected provider', () => {
    const fixture = setup({ managed: false, oidc: false, provider: { connected: false, canDisconnect: false } });
    expect(fixture.nativeElement.textContent).toContain('Example');
    expect(fixture.nativeElement.textContent).toContain('Not connected');
    expect(findByText(fixture.nativeElement, 'connect')).not.toBeNull();
  });

  it('disables Disconnect when the provider is the last way to sign in', () => {
    const fixture = setup({ managed: false, oidc: false, password: false, provider: { connected: true, canDisconnect: false } });
    const button = findByText(fixture.nativeElement, 'disconnect') as HTMLButtonElement | null;
    expect(button).not.toBeNull();
    expect(button!.disabled).toBe(true);
  });

  it('disconnects the provider on Disconnect', () => {
    const fixture = setup({ managed: false, oidc: false, provider: { connected: true, canDisconnect: true } });
    (findByText(fixture.nativeElement, 'disconnect') as HTMLButtonElement).click();
    fixture.detectChanges();
    expect(disconnects).toEqual(['example']);
    expect(notify.success).toHaveBeenCalledWith('Account disconnected');
  });

  it('reports a failed save as a toast with the server reason', () => {
    const writeFailure = new ApiError('Email already in use', 409);
    const fixture = setup({ managed: false, oidc: false, writeFailure });
    (findByText(fixture.nativeElement, 'save changes') as HTMLButtonElement).click();
    fixture.detectChanges();
    expect(notify.failure).toHaveBeenCalledWith('Failed to save profile', writeFailure);
    expect(fixture.componentInstance.saving()).toBe(false);
  });

  it('shows a success banner after connecting', () => {
    const fixture = setup({
      managed: false,
      oidc: false,
      provider: { connected: true, canDisconnect: true },
      query: { connected: '1' },
    });
    expect(fixture.nativeElement.textContent).toContain('Account connected.');
  });

  it('explains an account taken by another user', () => {
    const fixture = setup({
      managed: false,
      oidc: false,
      provider: { connected: false, canDisconnect: false },
      query: { connect_error: 'taken' },
    });
    expect(fixture.nativeElement.textContent).toContain('already connected to another user');
  });

  it('explains a connect that outlived its session', () => {
    const fixture = setup({
      managed: false,
      oidc: false,
      provider: { connected: false, canDisconnect: false },
      query: { connect_error: 'signed_out' },
    });
    expect(fixture.nativeElement.textContent).toContain('signed out');
  });
});

describe('ProfileComponent - delete confirmation', () => {
  it('asks for the username when the account has no password', async () => {
    const fixture = setup({ managed: false, oidc: false, password: false, provider: { connected: true, canDisconnect: false } });
    fixture.componentInstance.openDeleteDialog();
    fixture.detectChanges();
    await fixture.whenStable();
    expect(document.body.querySelector('#confirm-username')).not.toBeNull();
  });

  it('keeps a failed deletion inside the open dialog', () => {
    const fixture = setup({ managed: false, oidc: false, writeFailure: new ApiError('Wrong password', 403) });
    const component = fixture.componentInstance;
    component.openDeleteDialog();
    component.deletePassword = 'nope';
    component.deleteAccount();
    fixture.detectChanges();
    expect(component.showDeleteDialog()).toBe(true);
    expect(document.querySelector('.gr-dialog')?.textContent).toContain('Wrong password');
    expect((fixture.nativeElement as HTMLElement).textContent).not.toContain('Wrong password');
  });

  it('never offers an empty profile form to save after a failed load', () => {
    const opts: SetupOptions = { managed: false, oidc: false, loadFailure: new ApiError('database is down', 500) };
    const fixture = setup(opts);
    const root = fixture.nativeElement as HTMLElement;
    expect(root.textContent).toContain('Failed to load profile');
    expect(root.querySelector('#name')).toBeNull();

    opts.loadFailure = undefined;
    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')!.click();
    fixture.detectChanges();
    expect(root.textContent).not.toContain('Failed to load profile');
    expect(root.textContent).toContain('Personal Information');
  });
});
