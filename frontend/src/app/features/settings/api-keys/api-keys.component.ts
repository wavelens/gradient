/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, ElementRef, Injector, OnInit, afterNextRender, inject, signal, viewChild, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { catchError, of } from 'rxjs';
import { UserService } from '@core/services/user.service';
import { ProjectsService } from '@core/services/projects.service';
import { CachesService } from '@core/services/caches.service';
import { ApiError, apiErrorMessage } from '@core/services/api.service';
import { NotifyService } from '@core/services/notify.service';
import { ConfigService } from '@core/services/config.service';
import {
  BadgeComponent,
  ButtonComponent,
  CopyFieldComponent,
  DialogComponent,
  DividerComponent,
  FormFieldComponent,
  InputDirective,
  LoadStateComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
  SelectButtonComponent,
  SelectComponent,
} from '@gradient/ui/ui';
import { ManagedDisableDirective } from '@shared/access';
import { ConfirmService } from '@shared/confirm';
import { AccessState, ApiKeyInfo, ApiKeyVisibleProjectsAndCachesWithPermissionsResponse, PatchApiKeyRequest, PermissionEntry } from '@core/models';
import {
  buildGradientCliInstallNetrcCommand,
  buildNetrcEntryForServerUrl,
  buildNixConfCacheSubstituterAndKeyLines,
  type CacheClientSetupTexts,
  permissionLabel,
  serverTime,
  ServerTimestampToLocalDateTimePipe,
} from '@shared/text';
import { PermissionPickerComponent } from '@shared/ui';
import { CreateCacheDialogComponent, dialogFormChangesSinceOpen, fieldChecks, injectCanCreateCache, injectRevealFirstInvalid, isIpNetwork, splitLines } from '@shared/forms';
import { SettingsNavComponent } from '../settings-nav/settings-nav.component';

type ScopeType = 'none' | 'project' | 'cache';

interface KeyFields {
  name: string;
  permissions: string[];
  project: string | null;
  allowed_ips: string[];
}

const sameList = (a: readonly string[], b: readonly string[]): boolean =>
  a.length === b.length && a.every((value) => b.includes(value));

function keyChanges(key: ApiKeyInfo, next: KeyFields): PatchApiKeyRequest {
  const patch: PatchApiKeyRequest = {};
  if (next.name !== key.name) patch.name = next.name;
  if (!sameList(next.permissions, key.permissions)) patch.permissions = next.permissions;
  if (next.project !== (key.project ?? null)) patch.project = next.project;
  if (!sameList(next.allowed_ips, key.allowed_ips ?? [])) patch.allowed_ips = next.allowed_ips;
  return patch;
}

interface SelectOption {
  label: string;
  value: string | null;
}

@Component({
  selector: 'app-api-keys',
  standalone: true,
  imports: [
    SettingsNavComponent,
    CreateCacheDialogComponent,
    PermissionPickerComponent,
    CommonModule,
    ServerTimestampToLocalDateTimePipe,
    RouterModule,
    FormsModule,
    DialogComponent,
    ButtonComponent,
    InputDirective,
    DividerComponent,
    SelectComponent,
    SelectButtonComponent,
    LoadStateComponent,
    ManagedDisableDirective,
    PageLayoutComponent,
    FormFieldComponent,
    BadgeComponent,
    CopyFieldComponent,
    RowListComponent,
    RowComponent,
  ],
  templateUrl: './api-keys.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class ApiKeysComponent implements OnInit {
  private userService = inject(UserService);
  private projectsService = inject(ProjectsService);
  private cachesService = inject(CachesService);
  showCreateCache = signal(false);
  private createForKey = false;
  canCreateCache = injectCanCreateCache();
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);
  private host = inject<ElementRef<HTMLElement>>(ElementRef);
  private injector = inject(Injector);
  private apiKeysPageRoute = inject(ActivatedRoute);
  protected config = inject(ConfigService);

  loading = signal(true);
  loadError = signal<unknown>(null);
  creating = signal(false);
  saving = signal(false);

  keys = signal<ApiKeyInfo[]>([]);
  availablePermissions = signal<PermissionEntry[]>([]);
  availableCachePermissions = signal<PermissionEntry[]>([]);
  serverWidePermissions = signal<PermissionEntry[]>([]);
  projectOptions = signal<SelectOption[]>([]);
  signedInUserProjectPermissionsByProjectName = signal<ReadonlyMap<string, readonly string[]>>(new Map());
  cacheOptions = signal<SelectOption[]>([]);

  scopeOptions: { label: string; value: ScopeType }[] = [
    { label: 'Any project', value: 'none' },
    { label: 'Project', value: 'project' },
    { label: 'Cache', value: 'cache' },
  ];

  showDialog = signal(false);
  editingKey = signal<ApiKeyInfo | null>(null);
  showKeyDialog = signal(false);
  createdKeyValue = signal('');
  cacheClientSetupTexts = signal<CacheClientSetupTexts | null>(null);
  isApiKeyAccessDialogVisible = signal(false);
  apiKeyWithOpenReachDialog = signal<ApiKeyInfo | null>(null);
  apiKeyVisibleProjectsAndCachesWithPermissionsResponse = signal<ApiKeyVisibleProjectsAndCachesWithPermissionsResponse | null>(null);
  apiKeyReachLoadError = signal<unknown>(null);
  errorMessage = signal<string | null>(null);
  attempted = signal(false);
  keyNameConflictError = signal<{ name: string; message: string } | null>(null);
  allowedIpsTouched = signal(false);
  private keyForm = viewChild<ElementRef<HTMLElement>>('keyForm');
  private revealFirstInvalid = injectRevealFirstInvalid();

  formName = '';
  formExpiresInDays: number | null = null;
  formPermissions: Record<string, boolean> = {};
  formScope: ScopeType = 'none';
  formProject: string | null = null;
  formCache: string | null = null;
  formAllowedIps = '';
  formChanges = dialogFormChangesSinceOpen(() => [
    this.formName,
    this.formExpiresInDays,
    this.formPermissions,
    this.formScope,
    this.formProject,
    this.formCache,
    this.formAllowedIps,
  ]);

  ngOnInit(): void {
    this.loadKeys();
    this.userService.getApiKeyPermissions().subscribe({
      next: (response) => {
        this.availablePermissions.set(response.available_permissions);
        this.availableCachePermissions.set(response.availableCache);
        this.serverWidePermissions.set(response.server_wide_permissions ?? []);
      },
      error: () => {},
    });
    this.projectsService.getProjects(1, 100).subscribe({
      next: (paginated) => {
        this.projectOptions.set(paginated.items.map((o) => ({ label: o.display_name || o.name, value: o.name })));
        this.signedInUserProjectPermissionsByProjectName.set(new Map(paginated.items.map((o) => [o.name, o.permissions])));
      },
      error: () => {},
    });
    this.loadCacheOptions();
    this.openCreateApiKeyDialogForCacheQueryParameter();
  }

  private openCreateApiKeyDialogForCacheQueryParameter(): void {
    const cache = this.apiKeysPageRoute.snapshot.queryParamMap.get('cache');
    if (!cache) return;
    this.openCreateDialog();
    this.formScope = 'cache';
    this.formCache = cache;
    if (!this.cacheOptions().some((option) => option.value === cache)) {
      this.cacheOptions.update((options) => [{ label: cache, value: cache }, ...options]);
    }
    this.formPermissions = { viewCache: true, readStore: true };
    this.formChanges.mark();
  }

  loadSignedInUserProjectPermissionsForProject(project: string | null): void {
    if (!project) return;
    this.projectsService.getProject(project).subscribe({
      next: (response) => {
        this.signedInUserProjectPermissionsByProjectName.update((byName) => new Map(byName).set(project, response.permissions));
      },
      error: () => {},
    });
  }

  private loadCacheOptions(): void {
    this.cachesService.getCaches(1, 100).subscribe({
      next: (caches) => {
        this.cacheOptions.set(caches.items.map((c) => ({ label: c.display_name || c.name, value: c.name })));
      },
      error: () => {},
    });
  }

  createCacheInsteadOfPick(): void {
    this.createForKey = true;
    this.showDialog.set(false);
    this.showCreateCache.set(true);
  }

  cacheCreated(cache: string): void {
    this.createForKey = false;
    this.loadCacheOptions();
    this.formCache = cache;
    this.showDialog.set(true);
  }

  createCacheClosed(visible: boolean): void {
    if (visible || !this.createForKey) return;
    this.createForKey = false;
    this.showDialog.set(true);
  }

  loadKeys(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.userService.getApiKeys().subscribe({
      next: (keys) => {
        this.keys.set(keys);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  openCreateDialog(): void {
    this.editingKey.set(null);
    this.formName = '';
    this.formExpiresInDays = null;
    this.formScope = 'none';
    this.formProject = null;
    this.formCache = null;
    this.formPermissions = this.permissionTemplate(false);
    this.formPermissions['viewProject'] = true;
    this.formAllowedIps = '';
    this.errorMessage.set(null);
    this.attempted.set(false);
    this.keyNameConflictError.set(null);
    this.allowedIpsTouched.set(false);
    this.formChanges.mark();
    this.showDialog.set(true);
  }

  openEditDialog(key: ApiKeyInfo): void {
    this.editingKey.set(key);
    this.formName = key.name;
    this.formExpiresInDays = null;
    this.formScope = key.cache ? 'cache' : key.project ? 'project' : 'none';
    this.formProject = key.project ?? null;
    this.formCache = key.cache ?? null;
    if (key.cache && !this.cacheOptions().some((option) => option.value === key.cache)) {
      this.cacheOptions.update((options) => [{ label: key.cache!, value: key.cache! }, ...options]);
    }
    if (key.project && !this.projectOptions().some((option) => option.value === key.project)) {
      this.projectOptions.update((options) => [{ label: key.project!, value: key.project! }, ...options]);
    }
    this.loadSignedInUserProjectPermissionsForProject(key.project ?? null);
    this.formPermissions = this.permissionTemplate(false);
    for (const p of key.permissions) this.formPermissions[p] = true;
    this.formAllowedIps = (key.allowed_ips ?? []).join('\n');
    this.errorMessage.set(null);
    this.attempted.set(false);
    this.keyNameConflictError.set(null);
    this.allowedIpsTouched.set(false);
    this.formChanges.mark();
    this.showDialog.set(true);
  }

  invalidIps(): string[] {
    return splitLines(this.formAllowedIps).filter((entry) => !isIpNetwork(entry));
  }

  allowedIpsError(): string | null {
    const invalid = this.allowedIpsTouched() || this.attempted() ? this.invalidIps() : [];
    return invalid.length > 0 ? 'Not an IP address or CIDR: ' + invalid.join(', ') : null;
  }

  onScopeChange(): void {
    this.attempted.set(false);
    this.formProject = null;
    this.formCache = null;
    if (this.formScope !== 'none') {
      const serverWideIds = new Set(this.serverWidePermissions().map((permission) => permission.id));
      this.formPermissions = Object.fromEntries(Object.entries(this.formPermissions).filter(([id]) => !serverWideIds.has(id)));
    }
    const ids = this.activePermissions().map((permission) => permission.id);
    if (ids.every((id) => id in this.formPermissions)) return;
    const preselected = this.formScope === 'cache' ? ['viewCache', 'readStore'] : ['viewProject'];
    this.formPermissions = Object.fromEntries(ids.map((id) => [id, preselected.includes(id)]));
  }

  activePermissions(): PermissionEntry[] {
    return this.formScope === 'cache' ? this.availableCachePermissions() : this.availablePermissions();
  }

  /// An edit request cannot move a key between a cache pin and a project pin.
  scopeChoices(): { label: string; value: ScopeType }[] {
    const editing = this.editingKey();
    if (!editing) return this.scopeOptions;
    if (editing.cache) return this.scopeOptions.filter((option) => option.value === 'cache');
    return this.scopeOptions.filter((option) => option.value !== 'cache');
  }

  private permissionTemplate(value: boolean): Record<string, boolean> {
    const out: Record<string, boolean> = {};
    for (const p of this.activePermissions()) out[p.id] = value;
    return out;
  }

  getPermissionIdsUserCanGrantForKeyBeingEdited(): readonly string[] | null {
    if (this.formScope !== 'project' || !this.formProject) return null;
    return this.signedInUserProjectPermissionsByProjectName().get(this.formProject) ?? null;
  }

  selectedPermissions(): string[] {
    const grantable = this.getPermissionIdsUserCanGrantForKeyBeingEdited();
    return Object.entries(this.formPermissions)
      .filter(([id, on]) => on && (grantable?.includes(id) ?? true))
      .map(([id]) => id);
  }

  openApiKeyAccessDialog(key: ApiKeyInfo): void {
    this.apiKeyWithOpenReachDialog.set(key);
    this.apiKeyVisibleProjectsAndCachesWithPermissionsResponse.set(null);
    this.apiKeyReachLoadError.set(null);
    this.isApiKeyAccessDialogVisible.set(true);
    this.userService.getApiKeyAccessibleProjectsAndCaches(key.id).subscribe({
      next: (reach) => this.apiKeyVisibleProjectsAndCachesWithPermissionsResponse.set(reach),
      error: (error: unknown) => this.apiKeyReachLoadError.set(error),
    });
  }

  buildCurlCommandExampleForKeyScope(key: ApiKeyInfo): string {
    const path = key.cache ? `caches/${key.cache}` : key.project ? `projects/${key.project}` : 'projects';
    return `curl -H "Authorization: Bearer $GRADIENT_TOKEN" ${this.config.apiBaseUrl}/${path}`;
  }

  permissionIdsToCommaSeparatedLabels(permissions: readonly string[]): string {
    return permissions.map(permissionLabel).join(', ');
  }

  getApiKeyPermissionsLimitedToOwnerOnPinnedProject(key: ApiKeyInfo): string[] {
    const held = key.project ? this.signedInUserProjectPermissionsByProjectName().get(key.project) : undefined;
    return held ? key.permissions.filter((permission) => held.includes(permission)) : key.permissions;
  }

  nameError(): string | null {
    const conflict = this.keyNameConflictError();
    if (conflict && conflict.name === this.formName.trim()) return conflict.message;
    return this.attempted() && !this.formName.trim() ? 'Enter a name.' : null;
  }

  expiryError(): string | null {
    return this.editingKey() ? null : fieldChecks.positiveInteger(this.formExpiresInDays);
  }

  projectError(): string | null {
    return this.attempted() && this.formScope === 'project' && !this.formProject ? 'Select a project.' : null;
  }

  cacheError(): string | null {
    return this.attempted() && this.formScope === 'cache' && !this.formCache ? 'Select a cache.' : null;
  }

  permissionsError(): string | null {
    return this.attempted() && this.selectedPermissions().length === 0 ? 'Select at least one permission.' : null;
  }

  saveKey(): void {
    if (this.creating() || this.saving()) return;
    this.attempted.set(true);
    if (this.nameError() || this.expiryError() || this.projectError() || this.cacheError() || this.permissionsError() || this.allowedIpsError()) {
      this.revealFirstInvalid(() => this.keyForm()?.nativeElement);
      return;
    }
    const name = this.formName.trim();
    const perms = this.selectedPermissions();
    const project = this.formScope === 'project' ? this.formProject : null;
    const cache = this.formScope === 'cache' ? this.formCache : null;
    const allowedIps = splitLines(this.formAllowedIps);
    const editing = this.editingKey();
    this.errorMessage.set(null);
    if (editing) {
      const patch = keyChanges(editing, { name, permissions: perms, project, allowed_ips: allowedIps });
      if (Object.keys(patch).length === 0) {
        this.showDialog.set(false);
        return;
      }
      this.saving.set(true);
      this.userService
        .updateApiKey(editing.id, patch)
        .subscribe({
          next: () => {
            this.saving.set(false);
            this.showDialog.set(false);
            this.notify.success('API key saved');
            this.loadKeys();
          },
          error: (err: unknown) => {
            this.showApiKeyNameConflictOrErrorMessage(err, name, 'Failed to save key.');
            this.saving.set(false);
          },
        });
    } else {
      this.creating.set(true);
      this.userService
        .createApiKey(name, this.formExpiresInDays, perms, project, cache, allowedIps)
        .subscribe({
          next: (keyValue) => {
            this.creating.set(false);
            this.showDialog.set(false);
            this.createdKeyValue.set(keyValue);
            this.cacheClientSetupTexts.set(null);
            if (cache) this.fetchCacheSigningKeyAndSetApiKeyUsageInstructions(cache, keyValue);
            this.showKeyDialog.set(true);
            this.loadKeys();
          },
          error: (err: unknown) => {
            this.showApiKeyNameConflictOrErrorMessage(err, name, 'Failed to create key.');
            this.creating.set(false);
          },
        });
    }
  }

  private fetchCacheSigningKeyAndSetApiKeyUsageInstructions(cache: string, keyValue: string): void {
    const serverUrl = this.config.serveUrl;
    this.cachesService
      .getCache(cache)
      .pipe(catchError(() => of(null)))
      .subscribe((response) => {
        this.cacheClientSetupTexts.set({
          netrcCredentialsLine: buildNetrcEntryForServerUrl(serverUrl, keyValue),
          nixConfExtraSubstitutersAndTrustedPublicKeysLines: buildNixConfCacheSubstituterAndKeyLines(
            `${serverUrl}/cache/${cache}`,
            response?.public_key ? [response.public_key] : [],
          ),
          installNetrcCommand: buildGradientCliInstallNetrcCommand(serverUrl, cache, keyValue),
        });
      });
  }

  private showApiKeyNameConflictOrErrorMessage(err: unknown, name: string, fallback: string): void {
    if (err instanceof ApiError && err.code === 'already_exists') {
      this.keyNameConflictError.set({ name, message: err.message });
      this.revealFirstInvalid(() => this.keyForm()?.nativeElement);
      return;
    }
    this.errorMessage.set(apiErrorMessage(err, fallback));
  }

  revokeKey(key: ApiKeyInfo): void {
    this.confirm
      .confirm({
        title: 'Revoke API key',
        target: key.name,
        consequence: 'The key is invalid right away. Scripts and pipelines that use this key lose access.',
        confirmLabel: 'Revoke',
        failure: 'Revoking the API key failed.',
        severity: 'danger',
        action: () => this.userService.revokeApiKey(key.id),
      })
      .subscribe(() => {
        this.notify.success('API key revoked');
        this.loadKeys();
      });
  }

  deleteKey(key: ApiKeyInfo): void {
    this.confirm
      .confirm({
        title: 'Delete API key',
        target: key.name,
        consequence: 'It stops working and the list no longer shows it. Scripts and pipelines that use this key lose access.',
        confirmLabel: 'Delete',
        failure: 'Deleting the API key failed.',
        action: () => this.userService.deleteApiKey(key.name),
      })
      .subscribe(() => {
        this.notify.success('API key deleted');
        const index = this.keys().findIndex((listed) => listed.id === key.id);
        this.keys.update((keys) => keys.filter((listed) => listed.id !== key.id));
        afterNextRender(() => this.focusNextApiKeyRowOrHeadingAfterDelete(index), { injector: this.injector });
      });
  }

  private focusNextApiKeyRowOrHeadingAfterDelete(index: number): void {
    const host = this.host.nativeElement;
    const row = host.querySelectorAll('gr-row')[index];
    const target = row?.querySelector<HTMLElement>('button:not(:disabled)') ?? host.querySelector<HTMLElement>('h1');
    target?.focus();
  }

  getReadableApiKeyPermissionNamesForOwnerOnPinnedProject(key: ApiKeyInfo): string {
    const permissions = this.getApiKeyPermissionsLimitedToOwnerOnPinnedProject(key);
    return permissions.length === 0 ? 'No permissions' : this.permissionIdsToCommaSeparatedLabels(permissions);
  }

  scopeBadge(key: ApiKeyInfo): string {
    const label = (options: SelectOption[], name: string) => options.find((option) => option.value === name)?.label ?? name;
    if (key.cache) return `Cache ${label(this.cacheOptions(), key.cache)}`;
    if (key.project) return label(this.projectOptions(), key.project);
    return 'Any project';
  }

  isApiKeyExpired(key: ApiKeyInfo): boolean {
    return !!key.expires_at && serverTime(key.expires_at).getTime() <= Date.now();
  }

  rowAccess(key: ApiKeyInfo): AccessState {
    return { managed: key.managed, canEdit: true, canTrigger: true };
  }
}
