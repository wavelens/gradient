/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, DestroyRef, OnInit, computed, inject, input, output, signal } from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { FormsModule } from '@angular/forms';
import { Observable, finalize } from 'rxjs';
import {
  BadgeComponent,
  ButtonComponent,
  DialogComponent,
  FormFieldComponent,
  InputDirective,
  LoadStateComponent,
  RowComponent,
  RowListComponent,
  SettingsSectionComponent,
  SubmitDirective,
} from '@gradient/ui/ui';
import type { AccessState, PatchRoleRequest, PendingInvitationItem, PermissionEntry, TeamGrantEntry } from '@core/models';
import { TeamsService } from '@core/services/teams.service';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import { WritableDirective, countRoleHoldersAndBuildBreakdownText } from '@shared/access';
import { ConfirmService } from '@shared/confirm';
import { LabelHelpComponent, PermissionPickerComponent } from '@shared/ui';
import { permissionLabel } from '@shared/text';
import { dialogFormChangesSinceOpen } from '@shared/forms/unsaved-changes';
import type { PeopleListRow } from './access-reason-label';
import type { MembersPageRequests, ProjectOrCacheRole, ProjectOrCacheRoleRequests } from './members-page-requests';

const ROLE_NAME = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/;

interface RoleEditFormState {
  name: string;
  permissions: Record<string, boolean>;
}

@Component({
  selector: 'app-project-or-cache-roles-list',
  standalone: true,
  imports: [
    FormsModule,
    BadgeComponent,
    ButtonComponent,
    DialogComponent,
    FormFieldComponent,
    InputDirective,
    LoadStateComponent,
    RowComponent,
    RowListComponent,
    SettingsSectionComponent,
    SubmitDirective,
    WritableDirective,
    LabelHelpComponent,
    PermissionPickerComponent,
  ],
  templateUrl: './project-or-cache-roles-list.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class ProjectOrCacheRolesListComponent implements OnInit {
  private teams = inject(TeamsService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);
  private destroyRef = inject(DestroyRef);

  kind = input.required<'project' | 'cache'>();
  name = input.required<string>();
  roleRequests = input.required<ProjectOrCacheRoleRequests>();
  requests = input.required<MembersPageRequests>();
  roleAccess = input.required<AccessState>();
  canManageMembers = input(false);
  roles = input<readonly ProjectOrCacheRole[]>([]);
  availablePermissions = input<readonly PermissionEntry[]>([]);
  rolesLoading = input(false);
  rolesLoadError = input<unknown>(null);
  rows = input<readonly PeopleListRow[]>([]);
  roleListChanged = output<void>();

  savingRole = signal(false);
  deletingRole = signal(false);
  roleAttempted = signal(false);
  roleError = signal<string | null>(null);
  showRoleDialog = signal(false);
  editingRole = signal<ProjectOrCacheRole | null>(null);
  roleForm: RoleEditFormState = { name: '', permissions: {} };
  protected formChanges = dialogFormChangesSinceOpen(() => this.roleForm);

  private invitations = signal<PendingInvitationItem[]>([]);
  private teamGrantsReceivedForRoleHolderCount = signal<TeamGrantEntry[]>([]);
  holderSummaryByRoleName = computed(() =>
    countRoleHoldersAndBuildBreakdownText(
      this.roles().map((role) => role.name),
      {
        members: this.rows().flatMap((row) => (row.directRole ? [{ name: row.directRole }] : [])),
        teamGrants: this.teamGrantsReceivedForRoleHolderCount(),
        invitations: this.invitations(),
      },
    ),
  );

  ngOnInit(): void {
    const grants = this.kind() === 'project' ? this.teams.projectGrants(this.name()) : this.teams.cacheGrants(this.name());
    grants.pipe(takeUntilDestroyed(this.destroyRef)).subscribe({
      next: (items) => this.teamGrantsReceivedForRoleHolderCount.set(items.filter((grant) => grant.users)),
    });
    if (!this.canManageMembers()) return;
    this.requests().invitations().pipe(takeUntilDestroyed(this.destroyRef)).subscribe({
      next: (invitations) => this.invitations.set(invitations),
    });
  }

  openCreateRoleDialog(): void {
    this.resetRoleFormAndOpenRoleDialog(null, []);
  }

  openEditRoleDialog(role: ProjectOrCacheRole): void {
    if (!role.builtin) this.resetRoleFormAndOpenRoleDialog(role, role.permissions);
  }

  private resetRoleFormAndOpenRoleDialog(role: ProjectOrCacheRole | null, granted: readonly string[]): void {
    this.editingRole.set(role);
    const permissions = Object.fromEntries(this.availablePermissions().map((permission) => [permission.id, granted.includes(permission.id)]));
    this.roleForm = { name: role?.name ?? '', permissions };
    this.roleError.set(null);
    this.roleAttempted.set(false);
    this.formChanges.mark();
    this.showRoleDialog.set(true);
  }

  roleNameError(): string | null {
    const name = this.roleForm.name.trim();
    if (!name) return 'Enter a role name.';
    if (!ROLE_NAME.test(name)) {
      return "Use only lowercase characters, digits and hyphens. Don't put a hyphen at the start or end of the name.";
    }
    const editing = this.editingRole();
    const taken = this.roles().some((role) => role.id !== editing?.id && role.name.toLowerCase() === name.toLowerCase());
    return taken ? "Roles with this name already exist. Case doesn't matter in role names." : null;
  }

  saveRole(): void {
    if (this.savingRole()) return;
    this.roleAttempted.set(true);
    if (this.roleNameError()) return;

    const editing = this.editingRole();
    const name = this.roleForm.name.trim();
    const permissions = Object.entries(this.roleForm.permissions).filter(([, on]) => on).map(([id]) => id);
    let request: Observable<unknown>;
    if (editing) {
      const patch: PatchRoleRequest = {};
      if (name !== editing.name) patch.name = name;
      if ([...permissions].sort().join() !== [...editing.permissions].sort().join()) patch.permissions = permissions;
      if (Object.keys(patch).length === 0) {
        this.showRoleDialog.set(false);
        return;
      }
      request = this.roleRequests().updateRole(editing.id, patch);
    } else {
      request = this.roleRequests().createRole({ name, permissions });
    }

    this.savingRole.set(true);
    this.roleError.set(null);
    request.subscribe({
      next: () => {
        this.savingRole.set(false);
        this.showRoleDialog.set(false);
        this.notify.success('Role saved');
        this.roleListChanged.emit();
      },
      error: (error: unknown) => {
        this.roleError.set(apiErrorMessage(error, "Couldn't save roles"));
        this.savingRole.set(false);
      },
    });
  }

  deleteRole(role: ProjectOrCacheRole): void {
    if (role.builtin) return;
    this.confirm
      .confirm({
        title: 'Delete role',
        target: role.name,
        consequence: '',
        isIrreversible: true,
        confirmLabel: 'Delete',
        failure: "Couldn't delete role",
        action: () => {
          this.deletingRole.set(true);
          return this.roleRequests().deleteRole(role.id).pipe(finalize(() => this.deletingRole.set(false)));
        },
      })
      .subscribe(() => {
        this.notify.success('Role deleted');
        this.roleListChanged.emit();
      });
  }

  rolePermissionLabel(role: ProjectOrCacheRole): string {
    if (role.permissions.length === 0) return 'No permissions';
    return role.permissions.map(permissionLabel).join(', ');
  }
}
