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
import { ProjectsService } from '@core/services/projects.service';
import { UserService } from '@core/services/user.service';
import { ProjectAccessService } from '@core/services/project-access.service';
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
import {
  AccessState,
  MemberItem,
  RoleResponse_ProjectRoleOwner,
  PermissionEntry,
  PendingInvitationItem,
} from '@core/models';
import { permissionLabel } from '@shared/text';

interface RoleFormState {
  name: string;
  permissions: Record<string, boolean>;
}

@Component({
  selector: 'app-members-roles',
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
  templateUrl: './members-roles.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class MembersRolesComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private projectsService = inject(ProjectsService);
  private userService = inject(UserService);
  private projectAccess = inject(ProjectAccessService);
  private crumbs = inject(BreadcrumbsService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);

  access = signal<AccessState>({ managed: false, canEdit: false, canTrigger: false });

  projectName = '';
  breadcrumb = computed(() => this.crumbs.projectSettings(this.projectName, { label: 'Members & Roles' }));

  membersLoading = signal(true);
  rolesLoading = signal(true);
  membersLoadError = signal<unknown>(null);
  rolesLoadError = signal<unknown>(null);
  invitationsLoadError = signal<unknown>(null);
  addingMember = signal(false);
  updatingRole = signal<string | null>(null);
  savingRole = signal(false);

  members = signal<MemberItem[]>([]);
  invitations = signal<PendingInvitationItem[]>([]);
  roles = signal<RoleResponse_ProjectRoleOwner[]>([]);
  roleNames = computed(() => this.roles().map((r) => r.name));
  availablePermissions = signal<PermissionEntry[]>([]);
  userSuggestions = signal<string[]>([]);
  memberError = signal<string | null>(null);
  roleError = signal<string | null>(null);

  showAddMemberDialog = signal(false);
  showRoleDialog = signal(false);
  editingRole = signal<RoleResponse_ProjectRoleOwner | null>(null);

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
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.projectAccess.forProject(this.projectName).then((s) => this.access.set(s));
    this.loadRoles();
    this.loadMembers();
    this.loadInvitations();
  }

  loadMembers(): void {
    this.membersLoading.set(true);
    this.membersLoadError.set(null);
    this.projectsService.getMembers(this.projectName).subscribe({
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
    this.projectsService.getInvitations(this.projectName).subscribe({
      next: (invitations) => this.invitations.set(invitations),
      error: (error: unknown) => this.invitationsLoadError.set(error),
    });
  }

  revokeInvitation(username: string): void {
    this.confirm
      .confirm({
        title: 'Revoke invitation',
        target: username,
        consequence: `The invitation to ${username} no longer works. You can invite them again later.`,
        confirmLabel: 'Revoke',
        failure: 'Failed to revoke the invitation.',
        action: () => this.projectsService.revokeInvitation(this.projectName, username),
      })
      .subscribe(() => {
        this.notify.success('Invitation revoked');
        this.loadInvitations();
      });
  }

  loadRoles(): void {
    this.rolesLoading.set(true);
    this.rolesLoadError.set(null);
    this.projectsService.getRoles(this.projectName).subscribe({
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
    if (this.addingMember() || !this.newMember.user || !this.newMember.role) return;
    this.addingMember.set(true);
    this.memberError.set(null);
    this.projectsService
      .addMember(this.projectName, this.newMember.user, this.newMember.role)
      .subscribe({
        next: () => {
          this.addingMember.set(false);
          this.showAddMemberDialog.set(false);
          this.notify.success('Member added');
          this.loadMembers();
        },
        error: (error: unknown) => {
          this.memberError.set(apiErrorMessage(error, 'Failed to add member.'));
          this.addingMember.set(false);
        },
      });
  }

  updateMemberRole(username: string, role: string): void {
    this.updatingRole.set(username);
    this.projectsService
      .updateMemberRole(this.projectName, username, role)
      .subscribe({
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
        consequence: `${username} can no longer access this project.`,
        confirmLabel: 'Remove',
        failure: 'Failed to remove the member.',
        action: () => this.projectsService.removeMember(this.projectName, username),
      })
      .subscribe(() => {
        this.notify.success('Member removed');
        this.loadMembers();
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

  openEditRoleDialog(role: RoleResponse_ProjectRoleOwner): void {
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
      ? this.projectsService.updateRole(this.projectName, editing.id, data)
      : this.projectsService.createRole(this.projectName, data);
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

  deleteRole(role: RoleResponse_ProjectRoleOwner): void {
    if (role.builtin) return;
    this.confirm
      .confirm({
        title: 'Delete role',
        target: role.name,
        consequence: `${role.name} is gone for good.`,
        confirmLabel: 'Delete',
        failure: 'Failed to delete the role.',
        action: () => this.projectsService.deleteRole(this.projectName, role.id),
      })
      .subscribe(() => {
        this.notify.success('Role deleted');
        this.loadRoles();
      });
  }


  rolePermissionLabel(role: RoleResponse_ProjectRoleOwner): string {
    if (role.permissions.length === 0) return 'No permissions';
    return role.permissions.map(permissionLabel).join(', ');
  }
}
