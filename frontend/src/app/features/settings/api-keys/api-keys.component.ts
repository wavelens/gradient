/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { UserService } from '@core/services/user.service';
import { ProjectsService } from '@core/services/projects.service';
import { CachesService } from '@core/services/caches.service';
import { apiErrorMessage } from '@core/services/api.service';
import { NotifyService } from '@core/services/notify.service';
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
  TooltipDirective,
} from '@gradient/ui/ui';
import { ManagedDisableDirective } from '@shared/access';
import { ConfirmService } from '@shared/confirm';
import { AccessState, ApiKeyInfo, PatchApiKeyRequest, PermissionEntry } from '@core/models';
import { permissionLabel } from '@shared/text';
import { PermissionPickerComponent } from '@shared/ui';
import { fieldChecks, isIpNetwork, splitLines } from '@shared/forms';

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
    PermissionPickerComponent,
    CommonModule,
    RouterModule,
    FormsModule,
    DialogComponent,
    ButtonComponent,
    InputDirective,
    DividerComponent,
    SelectComponent,
    SelectButtonComponent,
    TooltipDirective,
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
  styleUrl: './api-keys.component.scss',
})
export class ApiKeysComponent implements OnInit {
  private userService = inject(UserService);
  private projectsService = inject(ProjectsService);
  private cachesService = inject(CachesService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);

  loading = signal(true);
  loadError = signal<unknown>(null);
  creating = signal(false);
  saving = signal(false);

  keys = signal<ApiKeyInfo[]>([]);
  availablePermissions = signal<PermissionEntry[]>([]);
  availableCachePermissions = signal<PermissionEntry[]>([]);
  projectOptions = signal<SelectOption[]>([{ label: 'Any project', value: null }]);
  cacheOptions = signal<SelectOption[]>([]);

  scopeOptions: { label: string; value: ScopeType }[] = [
    { label: 'None', value: 'none' },
    { label: 'Project', value: 'project' },
    { label: 'Cache', value: 'cache' },
  ];

  showDialog = signal(false);
  editingKey = signal<ApiKeyInfo | null>(null);
  showKeyDialog = signal(false);
  createdKeyValue = signal('');
  errorMessage = signal<string | null>(null);
  attempted = signal(false);

  formName = '';
  formExpiresInDays: number | null = null;
  formPermissions: Record<string, boolean> = {};
  formScope: ScopeType = 'none';
  formProject: string | null = null;
  formCache: string | null = null;
  formAllowedIps = '';

  ngOnInit(): void {
    this.loadKeys();
    this.userService.getApiKeyPermissions().subscribe({
      next: (response) => {
        this.availablePermissions.set(response.available_permissions);
        this.availableCachePermissions.set(response.availableCache);
      },
      error: () => {},
    });
    this.projectsService.getProjects(1, 100).subscribe({
      next: (paginated) => {
        const options: SelectOption[] = [
          { label: 'Any project', value: null },
          ...paginated.items.map((o) => ({ label: o.name, value: o.name })),
        ];
        this.projectOptions.set(options);
      },
      error: () => {},
    });
    this.cachesService.getCaches(1, 100).subscribe({
      next: (caches) => {
        this.cacheOptions.set(caches.items.map((c) => ({ label: c.display_name || c.name, value: c.name })));
      },
      error: () => {},
    });
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
    this.formPermissions = this.permissionTemplate(false);
    for (const p of key.permissions) this.formPermissions[p] = true;
    this.formAllowedIps = (key.allowed_ips ?? []).join('\n');
    this.errorMessage.set(null);
    this.attempted.set(false);
    this.showDialog.set(true);
  }

  invalidIps(): string[] {
    return splitLines(this.formAllowedIps).filter((entry) => !isIpNetwork(entry));
  }

  onScopeChange(): void {
    this.formProject = null;
    this.formCache = null;
    const isCacheScope = this.formScope === 'cache';
    const perms = isCacheScope ? this.availableCachePermissions() : this.availablePermissions();
    const out: Record<string, boolean> = {};
    for (const p of perms) out[p.id] = false;
    this.formPermissions = out;
    if (!isCacheScope) this.formPermissions['viewProject'] = true;
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

  selectedPermissions(): string[] {
    return Object.entries(this.formPermissions)
      .filter(([, on]) => on)
      .map(([id]) => id);
  }

  nameError(): string | null {
    return this.attempted() && !this.formName.trim() ? 'Enter a name.' : null;
  }

  expiryError(): string | null {
    return this.editingKey() ? null : fieldChecks.positiveInteger(this.formExpiresInDays);
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
    if (this.nameError() || this.expiryError() || this.cacheError() || this.permissionsError() || this.invalidIps().length > 0) {
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
            this.errorMessage.set(apiErrorMessage(err, 'Failed to save key.'));
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
            this.showKeyDialog.set(true);
            this.loadKeys();
          },
          error: (err: unknown) => {
            this.errorMessage.set(apiErrorMessage(err, 'Failed to create key.'));
            this.creating.set(false);
          },
        });
    }
  }

  revokeKey(key: ApiKeyInfo): void {
    this.confirm
      .confirm({
        title: 'Revoke API key',
        target: key.name,
        consequence: `${key.name} stops working immediately. Scripts and pipelines that use the key lose access.`,
        confirmLabel: 'Revoke',
        failure: 'Revoking the API key failed.',
        severity: 'warn',
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
        consequence: `${key.name} no longer appears in the list and stops working. Scripts and pipelines that use the key lose access.`,
        confirmLabel: 'Delete',
        failure: 'Deleting the API key failed.',
        action: () => this.userService.deleteApiKey(key.name),
      })
      .subscribe(() => {
        this.notify.success('API key deleted');
        this.loadKeys();
      });
  }

  permissionTooltip(key: ApiKeyInfo): string {
    if (key.permissions.length === 0) return 'No permissions';
    return key.permissions.map(permissionLabel).join(', ');
  }

  scopeBadge(key: ApiKeyInfo): string {
    if (key.cache) return `Cache ${key.cache}`;
    if (key.project) return key.project;
    return 'Any project';
  }

  rowAccess(key: ApiKeyInfo): AccessState {
    return { managed: key.managed, canEdit: true, canTrigger: true };
  }
}
