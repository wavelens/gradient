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
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
import { ApiKeysComponent } from './api-keys.component';
import { By } from '@angular/platform-browser';
import { CreateCacheDialogComponent } from '@shared/forms';
import { UserService } from '@core/services/user.service';
import { ApiError } from '@core/services/api.service';
import { ProjectsService } from '@core/services/projects.service';
import { NotifyService } from '@core/services/notify.service';
import { CachesService } from '@core/services/caches.service';
import { ConfigService } from '@core/services/config.service';
import { ApiKeyInfo, CacheResponse, ProjectResponse } from '@core/models';

const unmanagedKey: ApiKeyInfo = {
  id: 'k1',
  name: 'CI key',
  managed: false,
  permissions: ['viewProject'],
  cache_permissions: [],
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
  cache_permissions: [],
  project: null,
  created_at: '2026-01-01T00:00:00',
  last_used_at: null,
  expires_at: null,
  revoked_at: null,
  allowed_ips: [],
};

let notify = { success: vi.fn(), failure: vi.fn() };
const confirm = vi.fn();

function setup(
  keys: ApiKeyInfo[],
  user: object = {},
  projects: ({ name: string } & Record<string, unknown>)[] = [],
  queryParams: Record<string, string> = {},
): ComponentFixture<ApiKeysComponent> {
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
          getApiKeyPermissions: () => of({ available_permissions: [], availableCache: [], server_wide_permissions: [] }),
          ...user,
        },
      },
      {
        provide: ProjectsService,
        useValue: {
          getProjects: () => of({ items: projects, total: projects.length, page: 1, per_page: 100 }),
          getProject: (name: string) => of(projects.find((project) => project.name === name) ?? { name, permissions: [] }),
        },
      },
      { provide: NotifyService, useValue: notify },
      { provide: ConfirmService, useValue: { confirm } },
    ],
  });
  Object.defineProperty(TestBed.inject(ActivatedRoute).snapshot, 'queryParamMap', { value: convertToParamMap(queryParams) });
  TestBed.inject(ConfigService).serveUrl = 'https://ci.example.com';
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

  it('marks an expired key and offers only Delete', () => {
    const fixture = setup([{ ...unmanagedKey, expires_at: '2026-01-02 00:00:00' }]);
    const root = fixture.nativeElement as HTMLElement;
    expect(root.textContent).toContain('Expired');
    expect(root.textContent).not.toContain('Expires:');
    expect(buttonsByText(root, 'edit').length).toBe(0);
    expect(buttonsByText(root, 'revoke').length).toBe(0);
    expect(buttonsByText(root, 'delete').length).toBe(1);
  });

  it('shows the expiry date of a key that is still valid', () => {
    const fixture = setup([{ ...unmanagedKey, expires_at: '2999-01-01 00:00:00' }]);
    const root = fixture.nativeElement as HTMLElement;
    expect(root.textContent).toContain('Expires:');
    expect(root.textContent).not.toContain('Expired');
    expect(buttonsByText(root, 'revoke').length).toBe(1);
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
    expect(createApiKey).toHaveBeenCalledWith('deploy', null, ['viewProject'], [], null, null, ['10.0.0.0/8', '2001:db8::/32']);
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
    expect(root.querySelector('gr-row [slot="meta"]')?.textContent).toContain('view project');
    expect(root.querySelector('gr-row [slot="meta"]')?.textContent).not.toContain('delete project');
    expect(component.getReadableApiKeyPermissionNamesForOwnerOnPinnedProject(pinned)).toBe('view project');

    component.openEditDialog(pinned);
    fixture.detectChanges();
    const deleteBox = Array.from(document.querySelectorAll<HTMLLabelElement>('app-permission-picker label')).find((label) => label.textContent?.includes('Delete project'))!;
    expect(deleteBox.querySelector('input')!.disabled).toBe(true);
    expect(component.selectedPermissions()).toEqual(['viewProject']);
  });

  it('offers on a picked project only the permissions that GET /projects/{project} lists for the owner', () => {
    const fixture = setup([], {
      getApiKeyPermissions: () => of({ available_permissions: [{ id: 'viewProject', mutating: false }, { id: 'editTask', mutating: true }], availableCache: [], server_wide_permissions: [] }),
    }, [{ name: 'web', permissions: ['viewProject', 'editTask'] }]);
    const component = fixture.componentInstance;
    const getProject = vi.spyOn(TestBed.inject(ProjectsService), 'getProject').mockReturnValue(of({ name: 'web', permissions: ['viewProject'] } as unknown as ProjectResponse));
    component.openCreateDialog();
    component.formScope = 'project';
    component.onScopeChange();
    component.formProject = 'web';
    component.loadSignedInUserProjectPermissionsForProject('web');
    fixture.componentRef.changeDetectorRef.markForCheck();
    fixture.detectChanges();
    expect(getProject).toHaveBeenCalledWith('web');
    const editBox = Array.from(document.querySelectorAll<HTMLLabelElement>('app-permission-picker label')).find((label) => label.textContent?.includes('Edit task'))!;
    expect(editBox.querySelector('input')!.disabled).toBe(true);
    expect(component.getPermissionIdsUserCanGrantForKeyBeingEdited()).toEqual(['viewProject']);
  });

  it('offers Create project only for Any and drops it when the scope changes', () => {
    const fixture = setup([], {
      getApiKeyPermissions: () => of({
        available_permissions: [{ id: 'viewProject', mutating: false }],
        availableCache: [],
        server_wide_permissions: [{ id: 'createProject', mutating: true }],
      }),
    });
    const component = fixture.componentInstance;
    const pickerText = () => document.querySelector('app-permission-picker')?.textContent ?? '';
    component.openCreateDialog();
    fixture.detectChanges();
    expect(pickerText()).toContain('Create project');
    component.formPermissions = { ...component.formPermissions, createProject: true };
    expect(component.selectedPermissions()).toEqual(['viewProject', 'createProject']);

    component.formScope = 'project';
    component.onScopeChange();
    fixture.componentRef.changeDetectorRef.markForCheck();
    fixture.detectChanges();
    expect(pickerText()).not.toContain('Create project');
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
    component.cachePermissionCheckedById = { readCache: true };
    component.saveKey();
    fixture.detectChanges();
    expect(field('key-cache')).toContain('Select a cache.');
    expect(createApiKey).not.toHaveBeenCalled();
  });

  it('keeps the ticked permissions between scopes and pre-ticks read for a cache', () => {
    const { component } = openCreate();
    component.availablePermissions.set([{ id: 'viewProject', mutating: false, permission_explanation: '' }, { id: 'editTask', mutating: true, permission_explanation: '' }]);
    component.availableCachePermissions.set([{ id: 'viewCache', mutating: false, permission_explanation: '' }, { id: 'readStore', mutating: false, permission_explanation: '' }, { id: 'writeStore', mutating: true, permission_explanation: '' }]);
    component.formPermissions = { viewProject: true, editTask: true };
    component.formScope = 'project';
    component.onScopeChange();
    expect(component.formPermissions).toEqual({ viewProject: true, editTask: true });
    component.formScope = 'cache';
    component.onScopeChange();
    expect(component.cachePermissionCheckedById).toEqual({ viewCache: true, readStore: true, writeStore: false });
    component.formScope = 'unrestrictedScope';
    component.onScopeChange();
    expect(component.formPermissions).toEqual({ viewProject: true, editTask: true });
    expect(component.cachePermissionCheckedById).toEqual({ viewCache: true, readStore: true, writeStore: false });
  });

  it('Any scope shows project, cache and server-wide permission lists', () => {
    const fixture = setup([], {
      getApiKeyPermissions: () => of({
        available_permissions: [{ id: 'viewProject', mutating: false }],
        availableCache: [{ id: 'readStore', mutating: false }],
        server_wide_permissions: [{ id: 'createProject', mutating: true }],
      }),
    });
    fixture.componentInstance.openCreateDialog();
    fixture.detectChanges();
    const listText = (testId: string) => document.querySelector(`[data-testid="${testId}"]`)?.textContent ?? '';
    expect(fixture.componentInstance.scopeOptions.find((option) => option.value === 'unrestrictedScope')?.label).toBe('Any');
    expect(listText('key-project-permissions')).toContain('View project');
    expect(listText('key-project-permissions')).toContain('Create project');
    expect(listText('key-cache-permissions')).toContain('Read cache');
  });

  it('create with Any sends permissions and cache_permissions', () => {
    const { component, createApiKey } = openCreate();
    component.cachePermissionCheckedById = { readCache: true };
    component.saveKey();
    expect(createApiKey).toHaveBeenCalledWith('deploy', null, ['viewProject'], ['readCache'], null, null, []);
  });

  it('cache scope sends only cache_permissions', () => {
    const { component, createApiKey } = openCreate();
    component.formScope = 'cache';
    component.onScopeChange();
    component.formCache = 'main';
    component.cachePermissionCheckedById = { readCache: true };
    component.saveKey();
    expect(createApiKey).toHaveBeenCalledWith('deploy', null, [], ['readCache'], null, 'main', []);
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
    expect(createApiKey).toHaveBeenCalledWith('deploy', null, ['viewProject'], [], null, null, []);
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
    const cacheKey: ApiKeyInfo = { ...unmanagedKey, cache: 'nix-cache', permissions: [], cache_permissions: ['viewCache', 'readStore'] };
    const updateApiKey = vi.fn(() => of(cacheKey));
    const fixture = setup([cacheKey], { updateApiKey });
    const component = fixture.componentInstance;
    component.availableCachePermissions.set([
      { id: 'viewCache', mutating: false, permission_explanation: '' },
      { id: 'readStore', mutating: false, permission_explanation: '' },
      { id: 'writeStore', mutating: true, permission_explanation: '' },
    ]);
    component.openEditDialog(cacheKey);
    expect(component.formScope).toBe('cache');
    expect(component.formCache).toBe('nix-cache');
    expect(component.cachePermissionCheckedById).toEqual({ viewCache: true, readStore: true, writeStore: false });
    component.cachePermissionCheckedById = { ...component.cachePermissionCheckedById, writeStore: true };
    component.saveKey();
    expect(updateApiKey).toHaveBeenCalledWith('k1', { cache_permissions: ['viewCache', 'readStore', 'writeStore'] });
  });

  it('editing an Any key preselects both lists', () => {
    const anyKey: ApiKeyInfo = { ...unmanagedKey, permissions: ['viewProject', 'editTask'], cache_permissions: ['readStore'] };
    const updateApiKey = vi.fn(() => of(anyKey));
    const fixture = setup([anyKey], { updateApiKey });
    const component = fixture.componentInstance;
    component.availablePermissions.set([{ id: 'viewProject', mutating: false, permission_explanation: '' }, { id: 'editTask', mutating: true, permission_explanation: '' }]);
    component.availableCachePermissions.set([{ id: 'readStore', mutating: false, permission_explanation: '' }, { id: 'writeStore', mutating: true, permission_explanation: '' }]);
    component.openEditDialog(anyKey);
    expect(component.formScope).toBe('unrestrictedScope');
    expect(component.formPermissions).toEqual({ viewProject: true, editTask: true });
    expect(component.cachePermissionCheckedById).toEqual({ readStore: true, writeStore: false });
    component.cachePermissionCheckedById = { readStore: true, writeStore: true };
    component.saveKey();
    expect(updateApiKey).toHaveBeenCalledWith('k1', { cache_permissions: ['readStore', 'writeStore'] });
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

  it.each([
    { deleted: 'CI key', focused: 'the next key', expected: (root: HTMLElement) => root.querySelector('gr-row button') },
    { deleted: 'Nix key', focused: 'the page heading', expected: (root: HTMLElement) => root.querySelector('h1') },
  ])('moves focus to $focused after deleting $deleted', async ({ deleted, expected }) => {
    const fixture = setup([unmanagedKey, managedKey], { deleteApiKey: vi.fn(() => of('ok')) });
    const root = fixture.nativeElement as HTMLElement;
    document.body.appendChild(root);
    onTestFinished(() => root.remove());
    const row = Array.from(root.querySelectorAll('gr-row')).find((candidate) => candidate.textContent?.includes(deleted))!;
    fixture.componentInstance.deleteKey(deleted === unmanagedKey.name ? unmanagedKey : managedKey);
    fixture.detectChanges();
    await fixture.whenStable();
    expect(row.isConnected).toBe(false);
    expect(root.textContent).not.toContain(deleted);
    expect(document.activeElement).toBe(expected(root));
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

  it('opens the create dialog for the cache named in ?cache=', () => {
    const fixture = setup([], {}, [], { cache: 'main' });
    const component = fixture.componentInstance;
    expect(component.showDialog()).toBe(true);
    expect(component.formScope).toBe('cache');
    expect(component.formCache).toBe('main');
    expect(component.cacheOptions().map((option) => option.value)).toContain('main');
    expect(component.cachePermissionCheckedById).toEqual({ viewCache: true, readStore: true });
    expect(component.formChanges.changed()).toBe(false);
  });

  it('shows the netrc entry, nix.conf lines and install command with the new key of a cache-scoped key', () => {
    const createApiKey = vi.fn(() => of('secret-key'));
    const fixture = setup([], { createApiKey }, [], { cache: 'main' });
    const getCache = vi.spyOn(TestBed.inject(CachesService), 'getCache').mockReturnValue(of({ name: 'main', public_key: 'main:abc=' } as CacheResponse));
    const component = fixture.componentInstance;
    component.formName = 'laptop';
    component.saveKey();
    fixture.detectChanges();
    expect(createApiKey).toHaveBeenCalledWith('laptop', null, [], ['viewCache', 'readStore'], null, 'main', []);
    expect(getCache).toHaveBeenCalledWith('main');
    expect(component.cacheClientSetupTexts()).toEqual({
      netrcCredentialsLine: 'machine ci.example.com login gradient password secret-key',
      nixConfExtraSubstitutersAndTrustedPublicKeysLines: 'extra-substituters = https://ci.example.com/cache/main\nextra-trusted-public-keys = main:abc=',
      installNetrcCommand: expect.stringContaining('--server https://ci.example.com --token secret-key --cache main'),
    });
    expect(document.querySelector<HTMLInputElement>('[data-testid="created-key-netrc"] input')?.value).toBe('machine ci.example.com login gradient password secret-key');
  });

  it('shows no cache setup for a key that is not pinned to a cache', () => {
    const fixture = setup([], { createApiKey: () => of('secret-key') });
    const component = fixture.componentInstance;
    component.openCreateDialog();
    component.formName = 'deploy';
    component.saveKey();
    expect(component.cacheClientSetupTexts()).toBeNull();
  });
});

describe('ApiKeysComponent - key access', () => {
  const reach = {
    projects: [{ name: 'web', display_name: 'Web Shop', permissions: ['viewProject', 'editTask'] }],
    caches: [{ name: 'main', display_name: '', permissions: ['viewCache'] }],
    create_project: true,
  };

  it('lists the projects and caches a key reaches with their permissions, the API base URL and a curl example', () => {
    const getApiKeyAccessibleProjectsAndCaches = vi.fn(() => of(reach));
    const fixture = setup([unmanagedKey], { getApiKeyAccessibleProjectsAndCaches });
    buttonsByText(fixture.nativeElement, 'access')[0].click();
    fixture.detectChanges();
    expect(getApiKeyAccessibleProjectsAndCaches).toHaveBeenCalledWith('k1');
    const dialog = document.querySelector('.gr-dialog')!;
    const text = dialog.textContent?.replace(/\s+/g, ' ') ?? '';
    expect(dialog.querySelector('[data-testid="key-reach-projects"]')?.textContent).toContain('Web Shop');
    expect(text).toContain('view project, edit task');
    expect(dialog.querySelector('[data-testid="key-reach-caches"]')?.textContent).toContain('main');
    expect(text).toContain('This key can create projects.');
    const values = Array.from(dialog.querySelectorAll<HTMLInputElement | HTMLTextAreaElement>('input, textarea')).map((field) => field.value);
    expect(values).toContain('https://ci.example.com/api/v1');
    expect(values).toContain('curl -H "Authorization: Bearer $GRADIENT_TOKEN" https://ci.example.com/api/v1/projects');
  });

  it('points the curl example at the pinned project or cache', () => {
    const component = setup([]).componentInstance;
    expect(component.buildCurlCommandExampleForKeyScope({ ...unmanagedKey, project: 'web' })).toBe(
      'curl -H "Authorization: Bearer $GRADIENT_TOKEN" https://ci.example.com/api/v1/projects/web',
    );
    expect(component.buildCurlCommandExampleForKeyScope({ ...unmanagedKey, cache: 'main' })).toBe(
      'curl -H "Authorization: Bearer $GRADIENT_TOKEN" https://ci.example.com/api/v1/caches/main',
    );
  });

  it('shows a failed access load with Try Again that asks again', () => {
    const getApiKeyAccessibleProjectsAndCaches = vi.fn()
      .mockReturnValueOnce(throwError(() => new ApiError('database is down', 500)))
      .mockReturnValue(of({ projects: [], caches: [], create_project: false }));
    const fixture = setup([unmanagedKey], { getApiKeyAccessibleProjectsAndCaches });
    fixture.componentInstance.openApiKeyAccessDialog(unmanagedKey);
    fixture.detectChanges();
    expect(document.querySelector('.gr-dialog')?.textContent).toContain('Failed to load key access');
    document.querySelector<HTMLButtonElement>('.gr-dialog .gr-load-state__error button')!.click();
    fixture.detectChanges();
    expect(getApiKeyAccessibleProjectsAndCaches).toHaveBeenCalledTimes(2);
    expect(document.querySelector('.gr-dialog')?.textContent).toContain('This key cannot access private projects or caches.');
  });
});
