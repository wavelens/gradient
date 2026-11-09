/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { EMPTY, Subject, of, throwError } from 'rxjs';
import { ApiError } from '@core/services/api.service';
import { ConfirmService } from '@shared/confirm';
import { ProfileComponent } from './profile.component';
import { UserService } from '@core/services/user.service';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { ThemeService } from '@core/services/theme.service';
import { NotifyService } from '@core/services/notify.service';

let profileWrites: unknown[] = [];
let disconnects: string[] = [];
let notify = { success: vi.fn(), failure: vi.fn() };
let confirm = vi.fn(() => EMPTY);

interface SetupOptions {
  managed: boolean;
  oidc: boolean;
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
  confirm = vi.fn(() => EMPTY);
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
      { provide: ConfirmService, useValue: { confirm } },
      {
        provide: ActivatedRoute,
        useValue: { snapshot: { queryParamMap: convertToParamMap(opts.query ?? {}) } },
      },
      {
        provide: AuthService,
        useValue: {
          reloadUser: () => undefined,
          isAuthenticated: () => false,
          checkUsername: () => throwError(() => new ApiError('Username is already taken', 400)),
        },
      },
      { provide: ConfigService, useValue: { sshEnabled: false } },
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

function edit(fixture: ComponentFixture<ProfileComponent>, change: Record<string, string>): void {
  Object.assign(fixture.componentInstance.formData, change);
  fixture.componentRef.changeDetectorRef.markForCheck();
  fixture.detectChanges();
}

describe('ProfileComponent - edits', () => {
  const saveButton = (fixture: ComponentFixture<ProfileComponent>) =>
    findByText(fixture.nativeElement, 'save changes') as HTMLButtonElement;

  it('sends only the changed field, so an unchanged username is never checked again', () => {
    const fixture = setup({ managed: false, oidc: false });
    edit(fixture, { name: 'Alice Liddell' });
    saveButton(fixture).click();
    expect(profileWrites).toEqual([{ name: 'Alice Liddell' }]);
  });

  it.each([
    { field: 'username', value: '', message: 'Enter a username.' },
    { field: 'username', value: 'a', message: 'Use 3-50 characters.' },
    { field: 'name', value: ' ', message: 'Enter a full name.' },
    { field: 'email', value: '', message: 'Enter an email address.' },
    { field: 'email', value: 'alice', message: 'name@example.com' },
  ])('shows why $field = "$value" is refused and sends nothing', ({ field, value, message }) => {
    const fixture = setup({ managed: false, oidc: false });
    edit(fixture, { [field]: value });
    const input = (fixture.nativeElement as HTMLElement).querySelector(`#${field}`)!;
    expect(input.closest('gr-form-field')!.textContent).toContain(message);
    expect(saveButton(fixture).disabled).toBe(true);
    saveButton(fixture).click();
    input.dispatchEvent(new KeyboardEvent('keyup', { key: 'Enter', bubbles: true }));
    expect(profileWrites).toEqual([]);
  });

  it('shows a taken username under the field while typing, but never checks the saved one', async () => {
    const fixture = setup({ managed: false, oidc: false });
    const usernameField = () => (fixture.nativeElement as HTMLElement).querySelector('#username')!.closest('gr-form-field')!;
    edit(fixture, { username: 'Alice' });
    fixture.componentInstance.clearFieldServerErrorAndStartUsernameCheck('username');
    await new Promise((resolve) => setTimeout(resolve, 550));
    fixture.detectChanges();
    expect(fixture.componentInstance.serverErrorMessagesByField()).toEqual({});

    edit(fixture, { username: 'bob' });
    fixture.componentInstance.clearFieldServerErrorAndStartUsernameCheck('username');
    await new Promise((resolve) => setTimeout(resolve, 550));
    fixture.detectChanges();
    expect(usernameField().textContent).toContain('Username is already taken');
  });

  it('saves on Enter in a field', () => {
    const fixture = setup({ managed: false, oidc: false });
    edit(fixture, { email: 'alice@example.org' });
    (fixture.nativeElement as HTMLElement)
      .querySelector('#email')!
      .dispatchEvent(new KeyboardEvent('keyup', { key: 'Enter', bubbles: true }));
    expect(profileWrites).toEqual([{ email: 'alice@example.org' }]);
  });

  it('asks before leaving only with unsaved edits', () => {
    const fixture = setup({ managed: false, oidc: false });
    expect(fixture.componentInstance.canDeactivate()).toBe(true);
    expect(confirm).not.toHaveBeenCalled();
    edit(fixture, { name: 'Alice Liddell' });
    expect(fixture.componentInstance.canDeactivate()).not.toBe(true);
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ target: 'Profile settings' }));
  });

  it('counts a saved edit as the new state, for leaving and for the delete confirmation', () => {
    const fixture = setup({ managed: false, oidc: false });
    edit(fixture, { username: 'alice2' });
    saveButton(fixture).click();
    fixture.detectChanges();
    expect(fixture.componentInstance.canDeactivate()).toBe(true);
    expect(saveButton(fixture).disabled).toBe(true);
    expect(fixture.componentInstance.savedUsername()).toBe('alice2');
  });
});

describe('ProfileComponent - access gating', () => {
  it('enables Save Changes on an unmanaged, non-OIDC account once a field changes', () => {
    const fixture = setup({ managed: false, oidc: false });
    const save = findByText(fixture.nativeElement, 'save changes') as HTMLButtonElement | null;
    expect(save).not.toBeNull();
    expect(save!.disabled).toBe(true);
    edit(fixture, { name: 'Alice Liddell' });
    expect(save!.disabled).toBe(false);
  });

  it.each([
    { name: 'hide Save changes and Delete account for state-managed accounts', managed: true, oidc: false },
    { name: 'hide "Save changes" and "Delete account" for OIDC accounts', managed: false, oidc: true },
  ])('$name', ({ managed, oidc }) => {
    const fixture = setup({ managed, oidc });
    expect(findByText(fixture.nativeElement, 'save changes')).toBeNull();
    expect(findByText(fixture.nativeElement, 'delete account')).toBeNull();
    expect(fixture.nativeElement.querySelector('input#username')).toBeNull();
    expect(fixture.nativeElement.querySelectorAll('gr-field-row').length).toBe(3);
    expect(fixture.nativeElement.querySelector('gr-message-banner')).toBeNull();
  });

  it('sends one profile write for two Save Changes clicks', () => {
    const fixture = setup({ managed: false, oidc: false });
    const updateUserSettings = vi.fn(() => new Subject<string>());
    TestBed.inject(UserService).updateUserSettings = updateUserSettings;
    edit(fixture, { name: 'Alice Liddell' });
    const save = () => findByText(fixture.nativeElement, 'save changes') as HTMLButtonElement;
    save().click();
    fixture.detectChanges();
    save().click();
    expect(updateUserSettings).toHaveBeenCalledTimes(1);
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

  it('shows an email taken by another account under the field until it changes', () => {
    const writeFailure = new ApiError('Email already exists', 409, 'email_address_already_in_use');
    const fixture = setup({ managed: false, oidc: false, writeFailure });
    edit(fixture, { email: 'bob@example.org' });
    (findByText(fixture.nativeElement, 'save changes') as HTMLButtonElement).click();
    fixture.detectChanges();
    const emailField = () => (fixture.nativeElement as HTMLElement).querySelector('#email')!.closest('gr-form-field')!;
    expect(emailField().textContent).toContain('Email already exists');
    expect(notify.failure).not.toHaveBeenCalled();

    edit(fixture, { email: 'carol@example.org' });
    fixture.componentInstance.clearFieldServerErrorAndStartUsernameCheck('email');
    fixture.detectChanges();
    expect(emailField().textContent).not.toContain('Email already exists');
  });

  it('reports a failed save as a toast with the server reason', () => {
    const writeFailure = new ApiError('Email already in use', 409);
    const fixture = setup({ managed: false, oidc: false, writeFailure });
    edit(fixture, { email: 'alice@example.org' });
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

  it('names the saved account and checks against it, not against an unsaved username edit', async () => {
    const fixture = setup({ managed: false, oidc: false, password: false, provider: { connected: true, canDisconnect: false } });
    const component = fixture.componentInstance;
    component.formData.username = 'mallory';
    component.openDeleteDialog();
    component.deleteUsernameConfirm = 'mallory';
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
    const dialog = document.body.querySelector('.gr-dialog') as HTMLElement;
    expect(dialog.querySelector('p strong')?.textContent).toBe('alice');
    const confirmButton = Array.from(dialog.querySelectorAll<HTMLButtonElement>('.gr-dialog__footer button')).find(
      (b) => b.textContent?.trim() === 'Delete Account',
    )!;
    expect(confirmButton.disabled).toBe(true);
  });

  it('keeps a failed deletion inside the open dialog', () => {
    const fixture = setup({ managed: false, oidc: false, writeFailure: new ApiError('Wrong password', 403) });
    const component = fixture.componentInstance;
    component.openDeleteDialog();
    fixture.detectChanges();
    component.deletePassword = 'nope';
    component.deleteAccount();
    fixture.detectChanges();
    expect(component.showDeleteDialog()).toBe(true);
    expect(document.activeElement?.id).toBe('confirm-password');
    expect(document.querySelector('.gr-dialog')?.textContent).toContain('Wrong password');
    expect((fixture.nativeElement as HTMLElement).textContent).not.toContain('Wrong password');
  });

  it('keeps the delete dialog open while the deletion runs', () => {
    const fixture = setup({ managed: false, oidc: false });
    const deleteUser = vi.fn(() => new Subject<string>());
    TestBed.inject(UserService).deleteUser = deleteUser;
    const component = fixture.componentInstance;
    component.openDeleteDialog();
    component.deletePassword = 'secret';
    component.deleteAccount();
    fixture.detectChanges();
    document.querySelector('.gr-dialog')!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', keyCode: 27, bubbles: true }));
    document.querySelector<HTMLElement>('.gr-dialog-backdrop')!.click();
    fixture.detectChanges();
    expect(component.showDeleteDialog()).toBe(true);
    expect(document.querySelector('.gr-dialog')?.getAttribute('aria-busy')).toBe('true');
    expect(deleteUser).toHaveBeenCalledTimes(1);
  });

  it('forgets the deleted account and opens the sign-in page', () => {
    const fixture = setup({ managed: false, oidc: false });
    TestBed.inject(UserService).deleteUser = vi.fn(() => of('ok'));
    const forgetDeletedAccountAndGoToSignInPage = vi.fn();
    Object.assign(TestBed.inject(AuthService), { forgetDeletedAccountAndGoToSignInPage });
    const component = fixture.componentInstance;
    component.deletePassword = 'secret';
    component.deleteAccount();
    expect(notify.success).not.toHaveBeenCalled();
    expect(forgetDeletedAccountAndGoToSignInPage).toHaveBeenCalled();
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
