/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import {
  ProjectsService,
  ProjectMember,
  ProjectRole,
  PermissionDescriptor,
} from '@core/services/projects.service';
import { UserService } from '@core/services/user.service';
import { ProjectAccessService } from '@core/services/project-access.service';
import {
  AutoCompleteComponent,
  BadgeComponent,
  ButtonComponent,
  CheckboxComponent,
  DialogComponent,
  FormFieldComponent,
  IconComponent,
  InputDirective,
  LoadingSpinnerComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
  SelectComponent,
  SettingsSectionComponent,
} from '@shared/ui';
import { WritableDirective, ManagedDisableDirective } from '@shared/access';
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
    DialogComponent,
    ButtonComponent,
    InputDirective,
    AutoCompleteComponent,
    CheckboxComponent,
    LoadingSpinnerComponent,
    WritableDirective,
    ManagedDisableDirective,
    IconComponent,
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
  styleUrl: './members-roles.component.scss',
})
export class MembersRolesComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private projectsService = inject(ProjectsService);
  private userService = inject(UserService);
  private projectAccess = inject(ProjectAccessService);

  access = signal<AccessState>({ managed: false, canEdit: false, canTrigger: false });

  projectName = '';

  membersLoading = signal(true);
  rolesLoading = signal(true);
  addingMember = signal(false);
  removingMember = signal<string | null>(null);
  updatingRole = signal<string | null>(null);
  savingRole = signal(false);
  deletingRole = signal<string | null>(null);

  members = signal<ProjectMember[]>([]);
  roles = signal<ProjectRole[]>([]);
  availablePermissions = signal<PermissionDescriptor[]>([]);
  userSuggestions = signal<string[]>([]);
  memberError = signal<string | null>(null);
  roleError = signal<string | null>(null);

  showAddMemberDialog = signal(false);
  showRoleDialog = signal(false);
  editingRole = signal<ProjectRole | null>(null);

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
  }

  loadMembers(): void {
    this.membersLoading.set(true);
    this.projectsService.getMembers(this.projectName).subscribe({
      next: (members) => {
        this.members.set(members);
        this.membersLoading.set(false);
      },
      error: () => this.membersLoading.set(false),
    });
  }

  loadRoles(): void {
    this.rolesLoading.set(true);
    this.projectsService.getRoles(this.projectName).subscribe({
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
    this.projectsService
      .addMember(this.projectName, this.newMember.user, this.newMember.role)
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
    this.projectsService
      .updateMemberRole(this.projectName, username, role)
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
    this.projectsService.removeMember(this.projectName, username).subscribe({
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

  openEditRoleDialog(role: ProjectRole): void {
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

  deleteRole(role: ProjectRole): void {
    if (role.builtin) return;
    this.deletingRole.set(role.id);
    this.projectsService.deleteRole(this.projectName, role.id).subscribe({
      next: () => {
        this.deletingRole.set(null);
        this.loadRoles();
      },
      error: () => this.deletingRole.set(null),
    });
  }

  rolePermissionLabel(role: ProjectRole): string {
    if (role.permissions.length === 0) return 'No permissions';
    return role.permissions.join(', ');
  }
}
