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
import { ApiKey } from '@core/models';
import { PermissionDescriptor } from '@core/models/permission.model';
import {
  BadgeComponent,
  ButtonComponent,
  CheckboxComponent,
  CopyFieldComponent,
  DialogComponent,
  DividerComponent,
  EmptyStateComponent,
  FormFieldComponent,
  IconComponent,
  InputDirective,
  LoadingSpinnerComponent,
  MessageBannerComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
  SelectButtonComponent,
  SelectComponent,
  TooltipDirective,
} from '@shared/ui';
import { ManagedDisableDirective } from '@shared/access';
import { AccessState } from '@core/models';

type ScopeType = 'none' | 'project' | 'cache';

interface SelectOption {
  label: string;
  value: string | null;
}

@Component({
  selector: 'app-api-keys',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    DialogComponent,
    ButtonComponent,
    InputDirective,
    CheckboxComponent,
    DividerComponent,
    SelectComponent,
    SelectButtonComponent,
    TooltipDirective,
    LoadingSpinnerComponent,
    ManagedDisableDirective,
    IconComponent,
    PageLayoutComponent,
    FormFieldComponent,
    EmptyStateComponent,
    BadgeComponent,
    CopyFieldComponent,
    RowListComponent,
    RowComponent,
    MessageBannerComponent,
  ],
  templateUrl: './api-keys.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './api-keys.component.scss',
})
export class ApiKeysComponent implements OnInit {
  private userService = inject(UserService);
  private projectsService = inject(ProjectsService);
  private cachesService = inject(CachesService);

  loading = signal(true);
  creating = signal(false);
  saving = signal(false);
  deletingId = signal<string | null>(null);
  revokingId = signal<string | null>(null);

  keys = signal<ApiKey[]>([]);
  availablePermissions = signal<PermissionDescriptor[]>([]);
  availableCachePermissions = signal<PermissionDescriptor[]>([]);
  projectOptions = signal<SelectOption[]>([{ label: 'Any project', value: null }]);
  cacheOptions = signal<SelectOption[]>([]);

  scopeOptions: { label: string; value: ScopeType }[] = [
    { label: 'None', value: 'none' },
    { label: 'Project', value: 'project' },
    { label: 'Cache', value: 'cache' },
  ];

  showDialog = signal(false);
  editingKey = signal<ApiKey | null>(null);
  showKeyDialog = signal(false);
  createdKeyValue = signal('');
  errorMessage = signal<string | null>(null);

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
    this.userService.getApiKeys().subscribe({
      next: (keys) => {
        this.keys.set(keys);
        this.loading.set(false);
      },
      error: () => this.loading.set(false),
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
    this.showDialog.set(true);
  }

  openEditDialog(key: ApiKey): void {
    this.editingKey.set(key);
    this.formName = key.name;
    this.formExpiresInDays = null;
    this.formPermissions = this.permissionTemplate(false);
    for (const p of key.permissions) this.formPermissions[p] = true;
    if (key.cache) {
      this.formScope = 'cache';
      this.formCache = key.cache;
      this.formProject = null;
    } else if (key.project) {
      this.formScope = 'project';
      this.formProject = key.project;
      this.formCache = null;
    } else {
      this.formScope = 'none';
      this.formProject = null;
      this.formCache = null;
    }
    this.formAllowedIps = (key.allowed_ips ?? []).join('\n');
    this.errorMessage.set(null);
    this.showDialog.set(true);
  }

  private parseAllowedIps(): string[] {
    return this.formAllowedIps
      .split('\n')
      .map((s) => s.trim())
      .filter((s) => s.length > 0);
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

  activePermissions(): PermissionDescriptor[] {
    return this.formScope === 'cache' ? this.availableCachePermissions() : this.availablePermissions();
  }

  private permissionTemplate(value: boolean): Record<string, boolean> {
    const out: Record<string, boolean> = {};
    for (const p of this.availablePermissions()) out[p.id] = value;
    return out;
  }

  selectedPermissions(): string[] {
    return Object.entries(this.formPermissions)
      .filter(([, on]) => on)
      .map(([id]) => id);
  }

  saveKey(): void {
    const name = this.formName.trim();
    const perms = this.selectedPermissions();
    if (!name) {
      this.errorMessage.set('Name is required.');
      return;
    }
    if (perms.length === 0) {
      this.errorMessage.set('Select at least one permission.');
      return;
    }
    const project = this.formScope === 'project' ? this.formProject : null;
    const cache = this.formScope === 'cache' ? this.formCache : null;
    const allowedIps = this.parseAllowedIps();
    const editing = this.editingKey();
    if (editing) {
      this.saving.set(true);
      this.userService
        .updateApiKey(editing.id, {
          name,
          permissions: perms,
          project,
          cache,
          allowed_ips: allowedIps,
        })
        .subscribe({
          next: () => {
            this.saving.set(false);
            this.showDialog.set(false);
            this.loadKeys();
          },
          error: (err) => {
            this.errorMessage.set(err?.error?.message || 'Failed to save key.');
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
          error: (err) => {
            this.errorMessage.set(err?.error?.message || 'Failed to create key.');
            this.creating.set(false);
          },
        });
    }
  }

  revokeKey(key: ApiKey): void {
    this.revokingId.set(key.id);
    this.userService.revokeApiKey(key.id).subscribe({
      next: () => {
        this.revokingId.set(null);
        this.loadKeys();
      },
      error: () => this.revokingId.set(null),
    });
  }

  deleteKey(key: ApiKey): void {
    this.deletingId.set(key.id);
    this.userService.deleteApiKey(key.name).subscribe({
      next: () => {
        this.deletingId.set(null);
        this.loadKeys();
      },
      error: () => this.deletingId.set(null),
    });
  }

  copyKey(): void {
    navigator.clipboard.writeText(this.createdKeyValue());
  }

  permissionTooltip(key: ApiKey): string {
    if (key.permissions.length === 0) return 'No permissions';
    return key.permissions.join(', ');
  }

  scopeBadge(key: ApiKey): string {
    if (key.cache) return key.cache;
    if (key.project) return key.project;
    return 'Any project';
  }

  rowAccess(key: ApiKey): AccessState {
    return { managed: key.managed, canEdit: true, canTrigger: true };
  }
}
