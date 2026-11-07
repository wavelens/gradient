/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { CachesService } from '@core/services/caches.service';
import { UserService } from '@core/services/user.service';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import {
  AutoCompleteComponent,
  BadgeComponent,
  ButtonComponent,
  DialogComponent,
  FormFieldComponent,
  InputDirective,
  LoadStateComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
  SelectComponent,
  SettingsSectionComponent,
} from '@gradient/ui/ui';
import { LabelHelpComponent, PermissionPickerComponent } from '@shared/ui';
import { TeamGrantsComponent } from '@features/teams/team-grants/team-grants.component';
import { WritableDirective, ManagedDisableDirective } from '@shared/access';
import { ConfirmService } from '@shared/confirm';
import { injectCacheAccess } from '@core/resolvers/inject-access';
import {
  MemberItem,
  RoleResponse_CacheRoleOwner,
  PermissionEntry,
  PendingInvitationItem,
} from '@core/models';
import { permissionLabel } from '@shared/text';

interface RoleFormState {
  name: string;
  permissions: Record<string, boolean>;
}

@Component({
  selector: 'app-cache-members-roles',
  standalone: true,
  imports: [
    PermissionPickerComponent,
    TeamGrantsComponent,
    LabelHelpComponent,
    CommonModule,
    RouterModule,
    FormsModule,
    DialogComponent,
    ButtonComponent,
    InputDirective,
    AutoCompleteComponent,
    LoadStateComponent,
    WritableDirective,
    ManagedDisableDirective,
    PageLayoutComponent,
    FormFieldComponent,
    SettingsSectionComponent,
    RowListComponent,
    RowComponent,
    BadgeComponent,
    SelectComponent,
  ],
  templateUrl: './cache-members-roles.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class CacheMembersRolesComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private cachesService = inject(CachesService);
  private crumbs = inject(BreadcrumbsService);
  private userService = inject(UserService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);

  access = injectCacheAccess();

  cacheName = '';
  breadcrumb = computed(() => this.crumbs.cacheSettings(this.cacheName, { label: 'Members & Roles' }));

  membersLoading = signal(true);
  rolesLoading = signal(true);
  addingMember = signal(false);
  updatingRole = signal<string | null>(null);
  savingRole = signal(false);

  members = signal<MemberItem[]>([]);
  invitations = signal<PendingInvitationItem[]>([]);
  roles = signal<RoleResponse_CacheRoleOwner[]>([]);
  roleNames = computed(() => this.roles().map((r) => r.name));
  availablePermissions = signal<PermissionEntry[]>([]);
  userSuggestions = signal<string[]>([]);
  memberError = signal<string | null>(null);
  roleError = signal<string | null>(null);
  membersLoadError = signal<unknown>(null);
  rolesLoadError = signal<unknown>(null);
  invitationsLoadError = signal<unknown>(null);

  showAddMemberDialog = signal(false);
  showRoleDialog = signal(false);
  editingRole = signal<RoleResponse_CacheRoleOwner | null>(null);

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
    this.cacheName = this.route.snapshot.paramMap.get('cache') || '';
    this.loadRoles();
    this.loadMembers();
    this.loadInvitations();
  }

  loadMembers(): void {
    this.membersLoading.set(true);
    this.membersLoadError.set(null);
    this.cachesService.getMembers(this.cacheName).subscribe({
      next: (members) => {
        this.members.set(members);
        this.membersLoading.set(false);
      },
      error: (error: unknown) => {
        this.membersLoadError.set(error);
        this.membersLoading.set(false);
      },
    });
  }

  loadInvitations(): void {
    this.invitationsLoadError.set(null);
    this.cachesService.getInvitations(this.cacheName).subscribe({
      next: (invitations) => this.invitations.set(invitations),
      error: (error: unknown) => this.invitationsLoadError.set(error),
    });
  }

  revokeInvitation(username: string): void {
    this.confirm
      .confirm({
        title: 'Revoke Invitation',
        target: username,
        consequence: `The invitation to ${username} can no longer grant access. Another invitation can follow later.`,
        confirmLabel: 'Revoke',
        failure: 'Failed to revoke the invitation.',
        action: () => this.cachesService.revokeInvitation(this.cacheName, username),
      })
      .subscribe(() => {
        this.notify.success('Invitation revoked');
        this.loadInvitations();
      });
  }

  loadRoles(): void {
    this.rolesLoading.set(true);
    this.rolesLoadError.set(null);
    this.cachesService.getRoles(this.cacheName).subscribe({
      next: (response) => {
        this.roles.set(response.roles);
        this.availablePermissions.set(response.available_permissions);
        if (!this.newMember.role && response.roles.length > 0) {
          this.newMember.role = response.roles[0].name;
        }
        this.rolesLoading.set(false);
      },
      error: (error: unknown) => {
        this.rolesLoadError.set(error);
        this.rolesLoading.set(false);
      },
    });
  }

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
    if (!this.newMember.user || !this.newMember.role || this.addingMember()) return;
    this.addingMember.set(true);
    this.memberError.set(null);
    this.cachesService
      .addMember(this.cacheName, this.newMember.user, this.newMember.role)
      .subscribe({
        next: (outcome) => {
          this.addingMember.set(false);
          this.showAddMemberDialog.set(false);
          this.notify.success(outcome || 'Member added');
          this.loadMembers();
          this.loadInvitations();
        },
        error: (error: unknown) => {
          this.memberError.set(apiErrorMessage(error, 'Failed to add member.'));
          this.addingMember.set(false);
        },
      });
  }

  updateMemberRole(username: string, role: string): void {
    this.updatingRole.set(username);
    this.cachesService.updateMember(this.cacheName, username, role).subscribe({
      next: () => {
        this.updatingRole.set(null);
        this.loadMembers();
      },
      error: (error: unknown) => {
        this.updatingRole.set(null);
        this.notify.failure('Failed to change member role', error);
        this.loadMembers();
      },
    });
  }

  removeMember(username: string): void {
    this.confirm
      .confirm({
        title: 'Remove member',
        target: username,
        consequence: `${username} can no longer access this cache.`,
        confirmLabel: 'Remove',
        failure: 'Failed to remove member.',
        action: () => this.cachesService.removeMember(this.cacheName, username),
      })
      .subscribe(() => {
        this.notify.success('Member removed');
        this.loadMembers();
      });
  }

  openCreateRoleDialog(): void {
    this.editingRole.set(null);
    this.roleForm = {
      name: '',
      permissions: this.permissionTemplate(false),
    };
    this.roleError.set(null);
    this.showRoleDialog.set(true);
  }

  openEditRoleDialog(role: RoleResponse_CacheRoleOwner): void {
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
      ? this.cachesService.updateRole(this.cacheName, editing.id, data)
      : this.cachesService.createRole(this.cacheName, data);
    obs.subscribe({
      next: () => {
        this.savingRole.set(false);
        this.showRoleDialog.set(false);
        this.notify.success('Role saved');
        this.loadRoles();
      },
      error: (error: unknown) => {
        this.roleError.set(apiErrorMessage(error, 'Failed to save role.'));
        this.savingRole.set(false);
      },
    });
  }

  deleteRole(role: RoleResponse_CacheRoleOwner): void {
    if (role.builtin) return;
    this.confirm
      .confirm({
        title: 'Delete role',
        target: role.name,
        consequence: `Complete removal of ${role.name} from the cache.`,
        confirmLabel: 'Delete',
        failure: 'Failed to delete role.',
        action: () => this.cachesService.deleteRole(this.cacheName, role.id),
      })
      .subscribe(() => {
        this.notify.success('Role deleted');
        this.loadRoles();
      });
  }


  rolePermissionLabel(role: RoleResponse_CacheRoleOwner): string {
    if (role.permissions.length === 0) return 'No permissions';
    return role.permissions.map(permissionLabel).join(', ');
  }
}
