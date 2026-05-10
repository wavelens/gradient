/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { DialogModule } from 'primeng/dialog';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';
import { AutoCompleteModule } from 'primeng/autocomplete';
import { CheckboxModule } from 'primeng/checkbox';
import { DividerModule } from 'primeng/divider';
import {
  OrganizationsService,
  OrgMember,
  OrgRole,
  PermissionDescriptor,
} from '@core/services/organizations.service';
import { UserService } from '@core/services/user.service';
import { OrgAccessService } from '@core/services/org-access.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { AccessBannerComponent, WritableDirective, ManagedDisableDirective } from '@shared/access';
import { AccessState } from '@core/models';

interface RoleFormState {
  name: string;
  permissions: Record<string, boolean>;
}

@Component({
  selector: 'app-members-roles',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    DialogModule,
    ButtonModule,
    InputTextModule,
    AutoCompleteModule,
    CheckboxModule,
    DividerModule,
    LoadingSpinnerComponent,
    AccessBannerComponent,
    WritableDirective,
    ManagedDisableDirective,
  ],
  templateUrl: './members-roles.component.html',
  styleUrl: './members-roles.component.scss',
})
export class MembersRolesComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private organizationsService = inject(OrganizationsService);
  private userService = inject(UserService);
  private orgAccess = inject(OrgAccessService);

  access = signal<AccessState>({ managed: false, canEdit: false });

  orgName = '';

  membersLoading = signal(true);
  rolesLoading = signal(true);
  addingMember = signal(false);
  removingMember = signal<string | null>(null);
  updatingRole = signal<string | null>(null);
  savingRole = signal(false);
  deletingRole = signal<string | null>(null);

  members = signal<OrgMember[]>([]);
  roles = signal<OrgRole[]>([]);
  availablePermissions = signal<PermissionDescriptor[]>([]);
  userSuggestions = signal<string[]>([]);
  memberError = signal<string | null>(null);
  roleError = signal<string | null>(null);

  showAddMemberDialog = signal(false);
  showRoleDialog = signal(false);
  editingRole = signal<OrgRole | null>(null);

  newMember = { user: '', role: '' };
  roleForm: RoleFormState = { name: '', permissions: {} };

  roleNameById = computed(() => {
    const map: Record<string, string> = {};
    for (const role of this.roles()) {
      map[role.name] = role.name;
    }
    return map;
  });

  ngOnInit(): void {
    this.orgName = this.route.snapshot.paramMap.get('org') || '';
    this.orgAccess.forOrg(this.orgName).then((s) => this.access.set(s));
    this.loadRoles();
    this.loadMembers();
  }

  loadMembers(): void {
    this.membersLoading.set(true);
    this.organizationsService.getMembers(this.orgName).subscribe({
      next: (members) => {
        this.members.set(members);
        this.membersLoading.set(false);
      },
      error: () => this.membersLoading.set(false),
    });
  }

  loadRoles(): void {
    this.rolesLoading.set(true);
    this.organizationsService.getRoles(this.orgName).subscribe({
      next: (response) => {
        this.roles.set(response.roles);
        this.availablePermissions.set(response.available_permissions);
        if (!this.newMember.role && response.roles.length > 0) {
          this.newMember.role = response.roles[0].name;
        }
        this.rolesLoading.set(false);
      },
      error: () => this.rolesLoading.set(false),
    });
  }

  // ── Members ──────────────────────────────────────────────────────────────

  onUserSearch(event: { query: string }): void {
    if (!event.query.trim()) {
      this.userSuggestions.set([]);
      return;
    }
    this.userService.searchUsers(event.query).subscribe({
      next: (users) => this.userSuggestions.set(users.map((u) => u.username)),
      error: () => this.userSuggestions.set([]),
    });
  }

  openAddMemberDialog(): void {
    this.newMember = {
      user: '',
      role: this.roles()[0]?.name ?? '',
    };
    this.memberError.set(null);
    this.showAddMemberDialog.set(true);
  }

  addMember(): void {
    if (!this.newMember.user || !this.newMember.role) return;
    this.addingMember.set(true);
    this.memberError.set(null);
    this.organizationsService
      .addMember(this.orgName, this.newMember.user, this.newMember.role)
      .subscribe({
        next: () => {
          this.addingMember.set(false);
          this.showAddMemberDialog.set(false);
          this.loadMembers();
        },
        error: (err) => {
          this.memberError.set(
            err?.error?.message || err?.message || 'Failed to add member.'
          );
          this.addingMember.set(false);
        },
      });
  }

  updateMemberRole(username: string, role: string): void {
    this.updatingRole.set(username);
    this.organizationsService
      .updateMemberRole(this.orgName, username, role)
      .subscribe({
        next: () => {
          this.updatingRole.set(null);
          this.loadMembers();
        },
        error: () => {
          this.updatingRole.set(null);
          this.loadMembers();
        },
      });
  }

  removeMember(username: string): void {
    this.removingMember.set(username);
    this.organizationsService.removeMember(this.orgName, username).subscribe({
      next: () => {
        this.removingMember.set(null);
        this.loadMembers();
      },
      error: () => this.removingMember.set(null),
    });
  }

  // ── Roles ────────────────────────────────────────────────────────────────

  openCreateRoleDialog(): void {
    this.editingRole.set(null);
    this.roleForm = {
      name: '',
      permissions: this.permissionTemplate(false),
    };
    this.roleError.set(null);
    this.showRoleDialog.set(true);
  }

  openEditRoleDialog(role: OrgRole): void {
    if (role.builtin) return;
    this.editingRole.set(role);
    const map = this.permissionTemplate(false);
    for (const id of role.permissions) map[id] = true;
    this.roleForm = { name: role.name, permissions: map };
    this.roleError.set(null);
    this.showRoleDialog.set(true);
  }

  private permissionTemplate(value: boolean): Record<string, boolean> {
    const out: Record<string, boolean> = {};
    for (const p of this.availablePermissions()) out[p.id] = value;
    return out;
  }

  selectedPermissions(): string[] {
    return Object.entries(this.roleForm.permissions)
      .filter(([, on]) => on)
      .map(([id]) => id);
  }

  saveRole(): void {
    if (!this.roleForm.name.trim()) {
      this.roleError.set('Role name is required.');
      return;
    }
    this.savingRole.set(true);
    this.roleError.set(null);
    const editing = this.editingRole();
    const data = {
      name: this.roleForm.name.trim(),
      permissions: this.selectedPermissions(),
    };
    const obs = editing
      ? this.organizationsService.updateRole(this.orgName, editing.id, data)
      : this.organizationsService.createRole(this.orgName, data);
    obs.subscribe({
      next: () => {
        this.savingRole.set(false);
        this.showRoleDialog.set(false);
        this.loadRoles();
      },
      error: (err) => {
        this.roleError.set(
          err?.error?.message || err?.message || 'Failed to save role.'
        );
        this.savingRole.set(false);
      },
    });
  }

  deleteRole(role: OrgRole): void {
    if (role.builtin) return;
    this.deletingRole.set(role.id);
    this.organizationsService.deleteRole(this.orgName, role.id).subscribe({
      next: () => {
        this.deletingRole.set(null);
        this.loadRoles();
      },
      error: () => this.deletingRole.set(null),
    });
  }

  rolePermissionLabel(role: OrgRole): string {
    if (role.permissions.length === 0) return 'No permissions';
    return role.permissions.join(', ');
  }
}
