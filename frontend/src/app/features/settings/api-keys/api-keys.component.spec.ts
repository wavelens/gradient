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

function setup(keys: ApiKeyInfo[], user: object = {}, projects: object[] = []): ComponentFixture<ApiKeysComponent> {
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
        useValue: { getProjects: () => of({ items: projects, total: projects.length, page: 1, per_page: 100 }) },
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
    expect(revokeButtons[0].getAttribute('aria-disabled')).toBe('true');
    expect(deleteButtons.length).toBe(1);
    expect(deleteButtons[0].getAttribute('aria-disabled')).toBe('true');
  });

  it('renders Edit present-but-disabled for a managed key', () => {
    const fixture = setup([managedKey]);
    const editButtons = buttonsByText(fixture.nativeElement, 'edit');
    expect(editButtons.length).toBe(1);
    expect(editButtons[0].getAttribute('aria-disabled')).toBe('true');
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

  it('asks before Cancel throws away a typed key name', () => {
    const { fixture, component } = openCreate();
    const cancel = [...document.querySelectorAll<HTMLButtonElement>('[grDialogFooter] button')].find((b) => b.textContent?.includes('Cancel'))!;
    cancel.click();
    fixture.detectChanges();
    expect(document.querySelector('.gr-dialog__discard')?.textContent).toContain('Discard changes?');
    expect(component.showDialog()).toBe(true);
  });

  it('checks the allowed sources once the field loses focus, not while typing', () => {
    const { fixture, component, field } = openCreate();
    const ips = document.querySelector('#key-allowed-ips') as HTMLTextAreaElement;
    component.formAllowedIps = '10.0.0.0/33';
    fixture.componentRef.changeDetectorRef.markForCheck();
    fixture.detectChanges();
    expect(field('key-allowed-ips')).not.toContain('Not an IP address or CIDR');
    ips.dispatchEvent(new Event('blur'));
    fixture.detectChanges();
    expect(field('key-allowed-ips')).toContain('Not an IP address or CIDR: 10.0.0.0/33');
  });

  it('moves focus to the first invalid field when Create is refused', () => {
    Element.prototype.scrollIntoView = vi.fn();
    const { fixture, component } = openCreate();
    component.formName = ' ';
    buttonsByText(document.querySelector('.gr-dialog__footer')!, 'create')[0].click();
    fixture.detectChanges();
    expect(document.activeElement?.id).toBe('key-name');
  });

  it('labels projects by display name in the picker and the key badge', () => {
    const pinned = { ...unmanagedKey, project: 'web' };
    const fixture = setup([pinned], {}, [{ name: 'web', display_name: 'Web Shop' }]);
    const component = fixture.componentInstance;
    expect(component.projectOptions().map((option) => option.label)).toEqual(['Web Shop']);
    expect(component.scopeBadge(pinned)).toBe('Web Shop');
  });

  it('asks for a project under the Project scope instead of sending a key for any project', () => {
    const { fixture, component, createApiKey, field } = openCreate();
    component.formScope = 'project';
    component.onScopeChange();
    fixture.componentRef.changeDetectorRef.markForCheck();
    fixture.detectChanges();
    component.saveKey();
    fixture.detectChanges();
    expect(field('key-project')).toContain('Select a project.');
    expect(createApiKey).not.toHaveBeenCalled();
  });

  it('offers and counts only the permissions the owner holds on the pinned project', () => {
    const pinned = { ...unmanagedKey, project: 'web', permissions: ['viewProject', 'deleteProject'] };
    const fixture = setup([pinned], {
      getApiKeyPermissions: () => of({ available_permissions: [{ id: 'viewProject', mutating: false }, { id: 'deleteProject', mutating: true }], availableCache: [] }),
    }, [{ name: 'web', display_name: 'Web Shop', permissions: ['viewProject'] }]);
    const component = fixture.componentInstance;
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('gr-row [slot="meta"]')?.textContent).toContain('1 permission ');
    expect(component.permissionTooltip(pinned)).toBe('view project');

    component.openEditDialog(pinned);
    fixture.detectChanges();
    const deleteBox = Array.from(document.querySelectorAll<HTMLLabelElement>('app-permission-picker label')).find((label) => label.textContent?.includes('Delete project'))!;
    expect(deleteBox.querySelector('input')!.disabled).toBe(true);
    expect(component.selectedPermissions()).toEqual(['viewProject']);
  });

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

  it('forgets the refused Create when the scope changes', () => {
    const { fixture, component } = openCreate();
    component.formScope = 'project';
    component.onScopeChange();
    component.saveKey();
    expect(component.projectError()).toBe('Select a project.');
    component.formScope = 'cache';
    component.onScopeChange();
    fixture.detectChanges();
    expect(component.cacheError()).toBeNull();
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

  it('shows a taken key name under the name field until the name changes', () => {
    const reason = 'API key already exists';
    const fixture = setup([], { createApiKey: () => throwError(() => new ApiError(reason, 409, 'already_exists')) });
    const component = fixture.componentInstance;
    component.openCreateDialog();
    component.formName = 'deploy';
    component.formPermissions = { viewProject: true };
    component.saveKey();
    fixture.detectChanges();
    expect(component.nameError()).toBe(reason);
    expect(component.errorMessage()).toBeNull();
    component.formName = 'deploy-2';
    expect(component.nameError()).toBeNull();
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
