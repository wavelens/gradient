/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { EMPTY, Subject, of, throwError } from 'rxjs';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
import { ApiKeysComponent } from './api-keys.component';
import { By } from '@angular/platform-browser';
import { CreateCacheDialogComponent } from '@shared/forms';
import { UserService } from '@core/services/user.service';
import { ApiError } from '@core/services/api.service';
import { ProjectsService } from '@core/services/projects.service';
import { NotifyService } from '@core/services/notify.service';
import { ApiKeyInfo } from '@core/models';

const unmanagedKey: ApiKeyInfo = {
  id: 'k1',
  name: 'CI key',
  managed: false,
  permissions: ['viewProject'],
  project: null,
  created_at: '2026-01-01T00:00:00',
  last_used_at: null,
  expires_at: null,
  revoked_at: null,
  allowed_ips: [],
};

const managedKey: ApiKeyInfo = {
  id: 'k2',
  name: 'Nix key',
  managed: true,
  permissions: ['viewProject'],
  project: null,
  created_at: '2026-01-01T00:00:00',
  last_used_at: null,
  expires_at: null,
  revoked_at: null,
  allowed_ips: [],
};

let notify = { success: vi.fn(), failure: vi.fn() };
const confirm = vi.fn();

function setup(keys: ApiKeyInfo[], user: object = {}): ComponentFixture<ApiKeysComponent> {
  notify = { success: vi.fn(), failure: vi.fn() };
  confirm.mockReset().mockImplementation((request: ConfirmRequest) => request.action());
  TestBed.configureTestingModule({
    imports: [ApiKeysComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      {
        provide: UserService,
        useValue: {
          getApiKeys: () => of(keys),
          getApiKeyPermissions: () => of({ available_permissions: [], availableCache: [] }),
          ...user,
        },
      },
      {
        provide: ProjectsService,
        useValue: { getProjects: () => of({ items: [], total: 0, page: 1, per_page: 100 }) },
      },
      { provide: NotifyService, useValue: notify },
      { provide: ConfirmService, useValue: { confirm } },
    ],
  });
  const fixture = TestBed.createComponent(ApiKeysComponent);
  fixture.detectChanges();
  return fixture;
}

function buttonsByText(root: HTMLElement, text: string): HTMLButtonElement[] {
  const target = text.toLowerCase();
  return (Array.from(root.querySelectorAll('button')) as HTMLButtonElement[]).filter(
    (el) => (el.textContent ?? '').trim().toLowerCase().includes(target),
  );
}

describe('ApiKeysComponent - loading', () => {
  it('shows a failed load with Try Again instead of No API keys', () => {
    const getApiKeys = vi.fn()
      .mockReturnValueOnce(throwError(() => new ApiError('database is down', 500)))
      .mockReturnValue(of([unmanagedKey]));
    const fixture = setup([], { getApiKeys });
    const root = fixture.nativeElement as HTMLElement;
    expect(root.textContent).toContain('Failed to load API keys');
    expect(root.textContent).not.toContain('No API keys');
    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')?.click();
    fixture.detectChanges();
    expect(getApiKeys).toHaveBeenCalledTimes(2);
    expect(root.textContent).toContain('CI key');
  });
});

describe('ApiKeysComponent - managed-key gating', () => {
  it('renders Edit / Revoke / Delete enabled for an unmanaged key', () => {
    const fixture = setup([unmanagedKey]);
    const editButtons = buttonsByText(fixture.nativeElement, 'edit');
    const revokeButtons = buttonsByText(fixture.nativeElement, 'revoke');
    const deleteButtons = buttonsByText(fixture.nativeElement, 'delete');
    expect(editButtons.length).toBe(1);
    expect(editButtons[0].disabled).toBe(false);
    expect(revokeButtons.length).toBe(1);
    expect(revokeButtons[0].disabled).toBe(false);
    expect(deleteButtons.length).toBe(1);
    expect(deleteButtons[0].disabled).toBe(false);
  });

  it('renders Revoke / Delete present-but-disabled for a managed key', () => {
    const fixture = setup([managedKey]);
    const revokeButtons = buttonsByText(fixture.nativeElement, 'revoke');
    const deleteButtons = buttonsByText(fixture.nativeElement, 'delete');
    expect(revokeButtons.length).toBe(1);
    expect(revokeButtons[0].disabled).toBe(true);
    expect(deleteButtons.length).toBe(1);
    expect(deleteButtons[0].disabled).toBe(true);
  });

  it('renders Edit present-but-disabled for a managed key', () => {
    const fixture = setup([managedKey]);
    const editButtons = buttonsByText(fixture.nativeElement, 'edit');
    expect(editButtons.length).toBe(1);
    expect(editButtons[0].disabled).toBe(true);
  });
});

describe('ApiKeysComponent - allowed sources', () => {
  it('refuses to send entries that are not IP addresses or CIDRs', () => {
    const createApiKey = vi.fn(() => of('key'));
    const fixture = setup([], { createApiKey });
    const component = fixture.componentInstance;
    component.formName = 'deploy';
    component.formPermissions = { viewProject: true };
    component.formAllowedIps = '10.0.0.0/8\nci.example.com\n';
    expect(component.invalidIps()).toEqual(['ci.example.com']);
    component.saveKey();
    expect(createApiKey).not.toHaveBeenCalled();

    component.formAllowedIps = '10.0.0.0/8\n 2001:db8::/32 ';
    component.saveKey();
    expect(createApiKey).toHaveBeenCalledWith('deploy', null, ['viewProject'], null, null, ['10.0.0.0/8', '2001:db8::/32']);
  });
});

describe('ApiKeysComponent - field checks', () => {
  function openCreate(createApiKey = vi.fn(() => of('key'))) {
    const fixture = setup([], {
      createApiKey,
      getApiKeyPermissions: () => of({ available_permissions: [], availableCache: [{ id: 'readCache', mutating: false }] }),
    });
    const component = fixture.componentInstance;
    component.openCreateDialog();
    component.formName = 'deploy';
    fixture.detectChanges();
    const field = (id: string) => document.querySelector(`#${id}`)!.closest('gr-form-field')!.textContent ?? '';
    return { fixture, component, createApiKey, field };
  }

  it('shows an empty name under its field and sends nothing', () => {
    const { fixture, component, createApiKey, field } = openCreate();
    component.formName = ' ';
    buttonsByText(document.querySelector('.gr-dialog__footer')!, 'create')[0].click();
    fixture.detectChanges();
    expect(field('key-name')).toContain('Enter a name.');
    expect(createApiKey).not.toHaveBeenCalled();
  });

  it.each([0, -5, 1.5])('refuses an expiry of %s days before asking the server', (days) => {
    const { fixture, component, createApiKey, field } = openCreate();
    component.formExpiresInDays = days;
    fixture.componentRef.changeDetectorRef.markForCheck();
    fixture.detectChanges();
    expect(field('key-expires')).toContain('Enter an integer of 1 or higher.');
    component.saveKey();
    expect(createApiKey).not.toHaveBeenCalled();
  });

  it('asks for the cache of a cache-scoped key and sends nothing without it', () => {
    const { fixture, component, createApiKey, field } = openCreate();
    component.formScope = 'cache';
    component.onScopeChange();
    component.formPermissions = { readCache: true };
    component.saveKey();
    fixture.detectChanges();
    expect(field('key-cache')).toContain('Select a cache.');
    expect(createApiKey).not.toHaveBeenCalled();
  });

  it('creates the key on Enter in the name field', () => {
    const { createApiKey } = openCreate();
    document.querySelector('#key-name')!.dispatchEvent(new KeyboardEvent('keyup', { key: 'Enter', bubbles: true }));
    expect(createApiKey).toHaveBeenCalledWith('deploy', null, ['viewProject'], null, null, []);
  });
});

describe('ApiKeysComponent - edits', () => {
  it('sends only a changed name, so a rename keeps the scope and permissions the dialog cannot show', () => {
    const updateApiKey = vi.fn(() => of(unmanagedKey));
    const fixture = setup([unmanagedKey], { updateApiKey });
    const component = fixture.componentInstance;
    component.openEditDialog(unmanagedKey);
    component.formName = 'CI key 2';
    component.saveKey();
    expect(updateApiKey).toHaveBeenCalledWith('k1', { name: 'CI key 2' });
  });

  it('shows the cache and cache permissions of a cache-scoped key and keeps them on save', () => {
    const cacheKey: ApiKeyInfo = { ...unmanagedKey, cache: 'nix-cache', permissions: ['viewCache', 'readStore'] };
    const updateApiKey = vi.fn(() => of(cacheKey));
    const fixture = setup([cacheKey], { updateApiKey });
    const component = fixture.componentInstance;
    component.availableCachePermissions.set([
      { id: 'viewCache', mutating: false },
      { id: 'readStore', mutating: false },
      { id: 'writeStore', mutating: true },
    ]);
    component.openEditDialog(cacheKey);
    expect(component.formScope).toBe('cache');
    expect(component.formCache).toBe('nix-cache');
    expect(component.scopeChoices().map((option) => option.value)).toEqual(['cache']);
    expect(component.formPermissions).toEqual({ viewCache: true, readStore: true, writeStore: false });
    component.formPermissions = { ...component.formPermissions, writeStore: true };
    component.saveKey();
    expect(updateApiKey).toHaveBeenCalledWith('k1', { permissions: ['viewCache', 'readStore', 'writeStore'] });
  });

  it('closes without a request when nothing changed', () => {
    const updateApiKey = vi.fn(() => of(unmanagedKey));
    const fixture = setup([unmanagedKey], { updateApiKey });
    const component = fixture.componentInstance;
    component.openEditDialog(unmanagedKey);
    component.saveKey();
    expect(updateApiKey).not.toHaveBeenCalled();
    expect(component.showDialog()).toBe(false);
  });
});

describe('ApiKeysComponent - create errors', () => {
  it('shows the reason the server gave for refusing the key', () => {
    const reason = 'Permission manageProject exceeds your own permissions';
    const fixture = setup([], { createApiKey: () => throwError(() => new ApiError(reason, 403)) });
    const component = fixture.componentInstance;
    component.openCreateDialog();
    component.formName = 'deploy';
    component.formPermissions = { viewProject: true };
    component.saveKey();
    fixture.detectChanges();
    expect(component.errorMessage()).toBe(reason);
    expect(component.showDialog()).toBe(true);
    expect(document.querySelector('.gr-dialog')?.textContent).toContain(reason);
    expect((fixture.nativeElement as HTMLElement).textContent).not.toContain(reason);
  });
});

describe('ApiKeysComponent - dialogs', () => {
  it('keeps the create dialog open and sends one request while creating', () => {
    const createApiKey = vi.fn(() => new Subject<string>());
    const fixture = setup([], { createApiKey });
    const component = fixture.componentInstance;
    component.openCreateDialog();
    component.formName = 'deploy';
    component.formPermissions = { viewProject: true };
    component.saveKey();
    fixture.detectChanges();
    buttonsByText(document.querySelector('.gr-dialog__footer')!, 'create')[0].click();
    document.querySelector('.gr-dialog')!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', keyCode: 27, bubbles: true }));
    document.querySelector<HTMLElement>('.gr-dialog-backdrop')!.click();
    fixture.detectChanges();
    expect(createApiKey).toHaveBeenCalledTimes(1);
    expect(component.showDialog()).toBe(true);
    expect(document.querySelector('.gr-dialog')?.getAttribute('aria-busy')).toBe('true');
  });

  it('closes the new key only through Done', () => {
    const fixture = setup([]);
    const component = fixture.componentInstance;
    component.showKeyDialog.set(true);
    fixture.detectChanges();
    expect(document.querySelector('.gr-dialog__close')).toBeNull();
    document.querySelector('.gr-dialog')!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', keyCode: 27, bubbles: true }));
    document.querySelector<HTMLElement>('.gr-dialog-backdrop')!.click();
    fixture.detectChanges();
    expect(component.showKeyDialog()).toBe(true);

    buttonsByText(document.querySelector('.gr-dialog__footer')!, 'done')[0].click();
    expect(component.showKeyDialog()).toBe(false);
  });
});

describe('ApiKeysComponent - row actions', () => {
  it.each([
    { action: 'revoke', title: 'Revoke API key', method: 'revokeApiKey', sent: 'k1', toast: 'API key revoked' },
    { action: 'delete', title: 'Delete API key', method: 'deleteApiKey', sent: 'CI key', toast: 'API key deleted' },
  ])('asks before the $action, naming the key', ({ action, title, method, sent, toast }) => {
    const request = vi.fn(() => of('ok'));
    const fixture = setup([unmanagedKey], { [method]: request });
    buttonsByText(fixture.nativeElement, action)[0].click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title, target: 'CI key' }));
    expect(request).toHaveBeenCalledWith(sent);
    expect(notify.success).toHaveBeenCalledWith(toast);
  });

  it.each(['revoke', 'delete'])('sends no %s when the confirmation is cancelled', (action) => {
    const revokeApiKey = vi.fn(() => of('ok'));
    const deleteApiKey = vi.fn(() => of('ok'));
    const fixture = setup([unmanagedKey], { revokeApiKey, deleteApiKey });
    confirm.mockImplementation(() => EMPTY);
    buttonsByText(fixture.nativeElement, action)[0].click();
    expect(revokeApiKey).not.toHaveBeenCalled();
    expect(deleteApiKey).not.toHaveBeenCalled();
  });
});

describe('ApiKeysComponent - cache scope', () => {
  it('creates a missing cache from the key dialog and returns with it selected', () => {
    const fixture = setup([]);
    const component = fixture.componentInstance;
    component.openCreateDialog();
    component.formName = 'deploy';
    component.formScope = 'cache';
    component.createCacheInsteadOfPick();
    fixture.detectChanges();
    expect(component.showDialog()).toBe(false);
    fixture.debugElement.query(By.directive(CreateCacheDialogComponent)).componentInstance.created.emit('fresh');
    expect(component.showDialog()).toBe(true);
    expect(component.formCache).toBe('fresh');
    expect(component.formName).toBe('deploy');
  });
});
