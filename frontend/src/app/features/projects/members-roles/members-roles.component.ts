/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, linkedSignal, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { ProjectsService } from '@core/services/projects.service';
import { UserService } from '@core/services/user.service';
import { AuthService } from '@core/services/auth.service';
import { injectProject } from '@core/resolvers/project-access.resolver';
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
  TooltipDirective,
  type AutoCompleteSuggestion,
} from '@gradient/ui/ui';
import { LabelHelpComponent, PermissionPickerComponent } from '@shared/ui';
import { TeamGrantsComponent } from '@features/teams/team-grants/team-grants.component';
import {
  WritableDirective,
  ManagedDisableDirective,
  ServerConfigurationManagedBannerComponent,
  compareRoleNamesByBuiltInAccessThenName,
  countRoleHoldersAndBuildBreakdownText,
  formatMemberNameForConfirmDialog,
  getMemberRemovalConsequenceSentence,
  injectSaveMemberRoleChangeWithOwnRoleConfirmation,
  newRoleLacksCurrentRolePermission,
  requestIfProjectMember,
  formatMemberNameForAutocompleteSuggestion,
} from '@shared/access';
import { ConfirmService } from '@shared/confirm';
import { PENDING_INVITATION_REVOKE_FAILURE, getPendingInvitationRevocationConsequenceSentence } from '@shared/access/pending-invitation-revoke-dialog-texts';
import {
  accessTo,
  MemberItem,
  RoleResponse_ProjectRoleOwner,
  PermissionEntry,
  PendingInvitationItem,
  TeamGrantEntry,
  PatchRoleRequest,
} from '@core/models';
import { permissionLabel, ServerTimestampToLocalDateTimePipe } from '@shared/text';
import { links } from '@shared/routing';
import { Observable, finalize } from 'rxjs';

const ROLE_NAME = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/;

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
    ServerTimestampToLocalDateTimePipe,
    RouterModule,
    FormsModule,
    DialogComponent,
    ButtonComponent,
    InputDirective,
    AutoCompleteComponent,
    LoadStateComponent,
    WritableDirective,
    ManagedDisableDirective, ServerConfigurationManagedBannerComponent,
    PageLayoutComponent,
    FormFieldComponent,
    SettingsSectionComponent,
    RowListComponent,
    RowComponent,
    BadgeComponent,
    SelectComponent,
    TooltipDirective,
  ],
  templateUrl: './members-roles.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class MembersRolesComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private projectsService = inject(ProjectsService);
  private userService = inject(UserService);
  private router = inject(Router);
  private auth = inject(AuthService);
  private resolvedProject = injectProject();
  project = linkedSignal(() => this.resolvedProject());
  private crumbs = inject(BreadcrumbsService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);
  private saveMemberRoleChange = injectSaveMemberRoleChangeWithOwnRoleConfirmation();

  access = computed(() => accessTo(this.project(), 'manageMembers'));
  roleAccess = computed(() => accessTo(this.project(), 'manageRoles'));

  projectName = '';
  breadcrumb = computed(() => this.crumbs.projectSettings(this.projectName, { label: 'Members & Roles' }));

  membersLoading = signal(true);
  rolesLoading = signal(true);
  membersLoadError = signal<unknown>(null);
  rolesLoadError = signal<unknown>(null);
  invitationsLoadError = signal<unknown>(null);
  addingMember = signal(false);
  pendingMemberRoleSelection = signal<{ user: string; role: string } | null>(null);
  savingRole = signal(false);
  deletingRole = signal(false);
  roleAttempted = signal(false);

  members = signal<MemberItem[]>([]);
  invitations = signal<PendingInvitationItem[]>([]);
  roles = signal<RoleResponse_ProjectRoleOwner[]>([]);
  roleNames = computed(() => this.roles().map((r) => r.name));
  availablePermissions = signal<PermissionEntry[]>([]);
  userSuggestions = signal<AutoCompleteSuggestion[]>([]);
  memberError = signal<string | null>(null);
  roleError = signal<string | null>(null);

  showAddMemberDialog = signal(false);
  showRoleDialog = signal(false);
  editingRole = signal<RoleResponse_ProjectRoleOwner | null>(null);

  newMember = { user: '', role: '' };
  roleForm: RoleFormState = { name: '', permissions: {} };

  teamGrantsReceivedForRoleHolderCount = signal<TeamGrantEntry[]>([]);
  holderSummaryByRoleName = computed(() =>
    countRoleHoldersAndBuildBreakdownText(this.roleNames(), {
      members: this.members(),
      teamGrants: this.teamGrantsReceivedForRoleHolderCount(),
      invitations: this.invitations(),
    }),
  );
  soleAdminUsername = computed(() => {
    const admins = this.members().filter((member) => member.name === 'Admin');
    return admins.length === 1 ? admins[0].id : null;
  });

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
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
    if (!this.access().canEdit) return;
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
        target: formatMemberNameForConfirmDialog(this.invitations().find((invitation) => invitation.user === username)?.name, username),
        consequence: getPendingInvitationRevocationConsequenceSentence(username),
        confirmLabel: 'Revoke',
        failure: PENDING_INVITATION_REVOKE_FAILURE,
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
    requestIfProjectMember(this.project(), () => this.projectsService.getRoles(this.projectName)).subscribe({
      next: (response) => {
        this.roles.set([...response.roles].sort((a, b) => compareRoleNamesByBuiltInAccessThenName(a.name, b.name)));
        this.availablePermissions.set(response.available_permissions);
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
      next: (users) => this.userSuggestions.set(users.map(formatMemberNameForAutocompleteSuggestion)),
      error: () => this.userSuggestions.set([]),
    });
  }

  openAddMemberDialog(): void {
    this.newMember = { user: '', role: this.roles()[0]?.name ?? '' };
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
    const member = this.members().find((entry) => entry.id === username);
    this.pendingMemberRoleSelection.set({ user: username, role });
    this.saveMemberRoleChange({
      place: 'project',
      signedInUser: this.isSignedInUser(username),
      lowersAccess: newRoleLacksCurrentRolePermission(this.roles(), member?.name ?? '', role),
      target: formatMemberNameForConfirmDialog(member?.user_display_name, username),
      save: () => this.projectsService.updateMemberRole(this.projectName, username, role),
    })
      .pipe(finalize(() => this.pendingMemberRoleSelection.set(null)))
      .subscribe(() => {
        this.members.update((members) => members.map((entry) => (entry.id === username ? { ...entry, name: role } : entry)));
        this.notify.success('Member role saved');
        if (this.isSignedInUser(username)) this.navigateToProjectOrReloadProject(false);
      });
  }

  removeMember(username: string): void {
    this.confirm
      .confirm({
        title: 'Remove member',
        target: formatMemberNameForConfirmDialog(this.members().find((member) => member.id === username)?.user_display_name, username),
        consequence: getMemberRemovalConsequenceSentence('project', username, this.isSignedInUser(username)),
        confirmLabel: 'Remove',
        failure: 'Failed to remove the member.',
        action: () => this.projectsService.removeMember(this.projectName, username),
      })
      .subscribe(() => {
        this.notify.success('Member removed');
        if (this.isSignedInUser(username)) {
          this.navigateToProjectOrReloadProject(true);
          return;
        }
        this.loadMembers();
      });
  }

  navigateToProjectOrReloadProject(removed: boolean): void {
    if (removed) {
      void this.router.navigateByUrl(links.project(this.projectName));
      return;
    }
    this.projectsService.getProject(this.projectName).subscribe({
      next: (project) => this.project.set(project),
      error: () => void this.router.navigateByUrl(links.project(this.projectName)),
    });
  }

  private isSignedInUser(username: string): boolean {
    return this.auth.user()?.username === username;
  }

  // ── Roles ────────────────────────────────────────────────────────────────

  openCreateRoleDialog(): void {
    this.editingRole.set(null);
    this.roleForm = {
      name: '',
      permissions: this.permissionTemplate(false),
    };
    this.roleError.set(null);
    this.roleAttempted.set(false);
    this.showRoleDialog.set(true);
  }

  openEditRoleDialog(role: RoleResponse_ProjectRoleOwner): void {
    if (role.builtin) return;
    this.editingRole.set(role);
    const map = this.permissionTemplate(false);
    for (const id of role.permissions) map[id] = true;
    this.roleForm = { name: role.name, permissions: map };
    this.roleError.set(null);
    this.roleAttempted.set(false);
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

  get roleNameError(): string | null {
    const name = this.roleForm.name.trim();
    if (!name) return 'Enter a role name.';
    if (!ROLE_NAME.test(name)) {
      return "Use only lowercase characters, digits and hyphens. Don't put a hyphen at the start or end of the name.";
    }
    const editing = this.editingRole();
    const taken = this.roles().some((r) => r.id !== editing?.id && r.name.toLowerCase() === name.toLowerCase());
    return taken ? "Roles with this name already exist. Case doesn't matter in role names." : null;
  }

  saveRole(): void {
    if (this.savingRole()) return;
    this.roleAttempted.set(true);
    if (this.roleNameError) return;

    const editing = this.editingRole();
    const name = this.roleForm.name.trim();
    const permissions = this.selectedPermissions();
    let obs: Observable<unknown>;
    if (editing) {
      const patch: PatchRoleRequest = {};
      if (name !== editing.name) patch.name = name;
      if ([...permissions].sort().join() !== [...editing.permissions].sort().join()) patch.permissions = permissions;
      if (Object.keys(patch).length === 0) {
        this.showRoleDialog.set(false);
        return;
      }
      obs = this.projectsService.updateRole(this.projectName, editing.id, patch);
    } else {
      obs = this.projectsService.createRole(this.projectName, { name, permissions });
    }

    this.savingRole.set(true);
    this.roleError.set(null);
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
        consequence: '',
        isIrreversible: true,
        confirmLabel: 'Delete',
        failure: 'Failed to delete the role.',
        action: () => {
          this.deletingRole.set(true);
          return this.projectsService.deleteRole(this.projectName, role.id).pipe(finalize(() => this.deletingRole.set(false)));
        },
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
