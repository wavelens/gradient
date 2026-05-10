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
import { TooltipModule } from 'primeng/tooltip';
import { UserService } from '@core/services/user.service';
import { OrganizationsService } from '@core/services/organizations.service';
import { ApiKey } from '@core/models';
import { PermissionDescriptor } from '@core/models/permission.model';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { ManagedDisableDirective } from '@shared/access';
import { AccessState } from '@core/models';

interface OrgOption {
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

  loading = signal(true);
  creating = signal(false);
  saving = signal(false);
  deletingId = signal<string | null>(null);
  revokingId = signal<string | null>(null);

  keys = signal<ApiKey[]>([]);
  availablePermissions = signal<PermissionDescriptor[]>([]);
  orgOptions = signal<OrgOption[]>([{ label: 'Any organization', value: null }]);

  showDialog = signal(false);
  editingKey = signal<ApiKey | null>(null);
  showKeyDialog = signal(false);
  createdKeyValue = signal('');
  errorMessage = signal<string | null>(null);

  formName = '';
  formExpiresInDays: number | null = null;
  formPermissions: Record<string, boolean> = {};
  formOrganization: string | null = null;

  ngOnInit(): void {
    this.loadKeys();
    this.userService.getApiKeyPermissions().subscribe({
      next: (response) => this.availablePermissions.set(response.available_permissions),
      error: () => {},
    });
    this.organizationsService.getOrganizations(1, 100).subscribe({
      next: (paginated) => {
        const options: OrgOption[] = [
          { label: 'Any organization', value: null },
          ...paginated.items.map((o) => ({ label: o.name, value: o.name })),
        ];
        this.orgOptions.set(options);
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
    this.formPermissions = this.permissionTemplate(false);
    this.formPermissions['viewOrg'] = true;
    this.formOrganization = null;
    this.errorMessage.set(null);
    this.showDialog.set(true);
  }

  openEditDialog(key: ApiKey): void {
    this.editingKey.set(key);
    this.formName = key.name;
    this.formExpiresInDays = null;
    this.formPermissions = this.permissionTemplate(false);
    for (const p of key.permissions) this.formPermissions[p] = true;
    this.formOrganization = key.organization;
    this.errorMessage.set(null);
    this.showDialog.set(true);
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
    const editing = this.editingKey();
    if (editing) {
      this.saving.set(true);
      this.userService
        .updateApiKey(editing.id, {
          name,
          permissions: perms,
          organization: this.formOrganization,
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
        .createApiKey(name, this.formExpiresInDays, perms, this.formOrganization)
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

  rowAccess(key: ApiKey): AccessState {
    return { managed: key.managed, canEdit: true };
  }
}
