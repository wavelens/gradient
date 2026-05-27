/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { DialogModule } from 'primeng/dialog';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';
import { CheckboxModule } from 'primeng/checkbox';
import { DividerModule } from 'primeng/divider';
import { SelectModule } from 'primeng/select';
import { SelectButtonModule } from 'primeng/selectbutton';
import { TooltipModule } from 'primeng/tooltip';
import { UserService } from '@core/services/user.service';
import { OrganizationsService } from '@core/services/organizations.service';
import { CachesService } from '@core/services/caches.service';
import { ApiKey } from '@core/models';
import { PermissionDescriptor } from '@core/models/permission.model';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { ManagedDisableDirective } from '@shared/access';
import { AccessState } from '@core/models';

type ScopeType = 'none' | 'organization' | 'cache';

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
    DialogModule,
    ButtonModule,
    InputTextModule,
    CheckboxModule,
    DividerModule,
    SelectModule,
    SelectButtonModule,
    TooltipModule,
    LoadingSpinnerComponent,
    ManagedDisableDirective,
  ],
  templateUrl: './api-keys.component.html',
  styleUrl: './api-keys.component.scss',
})
export class ApiKeysComponent implements OnInit {
  private userService = inject(UserService);
  private organizationsService = inject(OrganizationsService);
  private cachesService = inject(CachesService);

  loading = signal(true);
  creating = signal(false);
  saving = signal(false);
  deletingId = signal<string | null>(null);
  revokingId = signal<string | null>(null);

  keys = signal<ApiKey[]>([]);
  availablePermissions = signal<PermissionDescriptor[]>([]);
  availableCachePermissions = signal<PermissionDescriptor[]>([]);
  orgOptions = signal<SelectOption[]>([{ label: 'Any organization', value: null }]);
  cacheOptions = signal<SelectOption[]>([]);

  scopeOptions: { label: string; value: ScopeType }[] = [
    { label: 'None', value: 'none' },
    { label: 'Organization', value: 'organization' },
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
  formOrganization: string | null = null;
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
    this.organizationsService.getOrganizations(1, 100).subscribe({
      next: (paginated) => {
        const options: SelectOption[] = [
          { label: 'Any organization', value: null },
          ...paginated.items.map((o) => ({ label: o.name, value: o.name })),
        ];
        this.orgOptions.set(options);
      },
      error: () => {},
    });
    this.cachesService.getCaches().subscribe({
      next: (caches) => {
        this.cacheOptions.set(caches.map((c) => ({ label: c.display_name || c.name, value: c.name })));
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
    this.formOrganization = null;
    this.formCache = null;
    this.formPermissions = this.permissionTemplate(false);
    this.formPermissions['viewOrg'] = true;
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
      this.formOrganization = null;
    } else if (key.organization) {
      this.formScope = 'organization';
      this.formOrganization = key.organization;
      this.formCache = null;
    } else {
      this.formScope = 'none';
      this.formOrganization = null;
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
    this.formOrganization = null;
    this.formCache = null;
    const isCacheScope = this.formScope === 'cache';
    const perms = isCacheScope ? this.availableCachePermissions() : this.availablePermissions();
    const out: Record<string, boolean> = {};
    for (const p of perms) out[p.id] = false;
    this.formPermissions = out;
    if (!isCacheScope) this.formPermissions['viewOrg'] = true;
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
    const organization = this.formScope === 'organization' ? this.formOrganization : null;
    const cache = this.formScope === 'cache' ? this.formCache : null;
    const allowedIps = this.parseAllowedIps();
    const editing = this.editingKey();
    if (editing) {
      this.saving.set(true);
      this.userService
        .updateApiKey(editing.id, {
          name,
          permissions: perms,
          organization,
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
        .createApiKey(name, this.formExpiresInDays, perms, organization, cache, allowedIps)
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
    if (key.organization) return key.organization;
    return 'Any org';
  }

  rowAccess(key: ApiKey): AccessState {
    return { managed: key.managed, canEdit: true, canTrigger: true };
  }
}
