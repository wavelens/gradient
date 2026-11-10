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
import { CachesService } from '@core/services/caches.service';
import { UserService } from '@core/services/user.service';
import { AuthService } from '@core/services/auth.service';
import { NotifyService } from '@core/services/notify.service';
import { ApiError, apiErrorMessage } from '@core/services/api.service';
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
  buildMemberSoleAdminDisabledReasonTextForUsername,
  compareRoleNamesByBuiltInAccessThenName,
  countRoleHoldersAndBuildBreakdownText,
  formatMemberNameForConfirmDialog,
  getMemberRemovalConsequenceSentence,
  injectSaveMemberRoleChangeWithOwnRoleConfirmation,
  newRoleLacksCurrentRolePermission,
  createNoAccessLoadStateError,
  isViewerCacheMember,
  requestIfCacheMember,
  formatMemberNameForAutocompleteSuggestion,
} from '@shared/access';
import { ConfirmService } from '@shared/confirm';
import { PENDING_INVITATION_REVOKE_FAILURE, getPendingInvitationRevocationConsequenceSentence } from '@shared/access/pending-invitation-revoke-dialog-texts';
import { runReloadIntervalWhileTabVisible } from '@shared/paging';
import { EMPTY, finalize, merge, tap } from 'rxjs';
import { injectCacheAccessData } from '@core/resolvers/inject-access';
import { links } from '@shared/routing';
import {
  accessTo,
  MemberItem,
  RoleResponse_CacheRoleOwner,
  PermissionEntry,
  PendingInvitationItem,
  TeamGrantEntry,
} from '@core/models';
import { permissionLabel, ServerTimestampToLocalDateTimePipe } from '@shared/text';

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
  private saveMemberRoleChange = injectSaveMemberRoleChangeWithOwnRoleConfirmation();

  private cacheAccess = injectCacheAccessData();
  private router = inject(Router);
  private auth = inject(AuthService);
  protected cache = linkedSignal(() => this.cacheAccess()?.cache);
  access = computed(() => accessTo(this.cache(), 'manageCacheMembers'));
  roleAccess = computed(() => accessTo(this.cache(), 'manageCacheRoles'));
  viewerIsCacheMember = computed(() => isViewerCacheMember(this.cache()));
  readonly missingMembershipError = createNoAccessLoadStateError('cache', 'members');
  private membersOnlyErrorOnForbidden = (error: unknown): unknown =>
    error instanceof ApiError && error.status === 403 ? this.missingMembershipError : error;

  cacheName = '';
  breadcrumb = computed(() => this.crumbs.cacheSettings(this.cacheName, { label: 'Members & Roles' }));

  membersLoading = signal(true);
  rolesLoading = signal(true);
  addingMember = signal(false);
  pendingMemberRoleSelection = signal<{ user: string; role: string } | null>(null);
  savingRole = signal(false);
  deletingRole = signal(false);

  members = signal<MemberItem[]>([]);
  invitations = signal<PendingInvitationItem[]>([]);
  roles = signal<RoleResponse_CacheRoleOwner[]>([]);
  roleNames = computed(() => this.roles().map((r) => r.name));
  availablePermissions = signal<PermissionEntry[]>([]);
  userSuggestions = signal<AutoCompleteSuggestion[]>([]);
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

  teamGrantsReceivedForRoleHolderCount = signal<TeamGrantEntry[]>([]);
  holderSummaryByRoleName = computed(() =>
    countRoleHoldersAndBuildBreakdownText(this.roleNames(), {
      members: this.members(),
      teamGrants: this.teamGrantsReceivedForRoleHolderCount(),
      invitations: this.invitations(),
    }),
  );
  memberOnlyAdminDisabledReason = computed(() =>
    buildMemberSoleAdminDisabledReasonTextForUsername(
      'cache',
      this.members().filter((member) => member.name === 'Admin').map((member) => member.id),
    ),
  );

  constructor() {
    runReloadIntervalWhileTabVisible(() => merge(
      requestIfCacheMember(this.cache(), () => this.cachesService.getMembers(this.cacheName), 'viewCache').pipe(tap((members) => {
        this.members.set(members);
        this.membersLoadError.set(null);
      })),
      this.access().canEdit
        ? this.cachesService.getInvitations(this.cacheName).pipe(tap((invitations) => {
          this.invitations.set(invitations);
          this.invitationsLoadError.set(null);
        }))
        : EMPTY,
    ));
  }

  ngOnInit(): void {
    this.cacheName = this.route.snapshot.paramMap.get('cache') || '';
    this.loadRoles();
    this.loadMembers();
    this.loadInvitations();
  }

  loadMembers(): void {
    this.membersLoading.set(true);
    this.membersLoadError.set(null);
    requestIfCacheMember(this.cache(), () => this.cachesService.getMembers(this.cacheName), 'viewCache').subscribe({
      next: (members) => {
        this.members.set(members);
        this.membersLoading.set(false);
      },
      error: (error: unknown) => {
        this.membersLoadError.set(this.membersOnlyErrorOnForbidden(error));
        this.membersLoading.set(false);
      },
    });
  }

  loadInvitations(): void {
    if (!this.access().canEdit) return;
    this.invitationsLoadError.set(null);
    this.cachesService.getInvitations(this.cacheName).subscribe({
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
    requestIfCacheMember(this.cache(), () => this.cachesService.getRoles(this.cacheName)).subscribe({
      next: (response) => {
        this.roles.set([...response.roles].sort((a, b) => compareRoleNamesByBuiltInAccessThenName(a.name, b.name)));
        this.availablePermissions.set(response.available_permissions);
        this.rolesLoading.set(false);
      },
      error: (error: unknown) => {
        this.rolesLoadError.set(this.membersOnlyErrorOnForbidden(error));
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
      next: (users) => {
        const memberIds = new Set(this.members().map((member) => member.id));
        this.userSuggestions.set(users.filter((user) => !memberIds.has(user.username)).map(formatMemberNameForAutocompleteSuggestion));
      },
      error: () => this.userSuggestions.set([]),
    });
  }

  openAddMemberDialog(): void {
    this.newMember = { user: '', role: this.roles()[0]?.name ?? '' };
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
    const member = this.members().find((entry) => entry.id === username);
    this.pendingMemberRoleSelection.set({ user: username, role });
    this.saveMemberRoleChange({
      place: 'cache',
      signedInUser: this.isSignedInUser(username),
      lowersAccess: newRoleLacksCurrentRolePermission(this.roles(), member?.name ?? '', role),
      target: formatMemberNameForConfirmDialog(member?.user_display_name, username),
      save: () => this.cachesService.updateMember(this.cacheName, username, role),
    })
      .pipe(finalize(() => this.pendingMemberRoleSelection.set(null)))
      .subscribe(() => {
        this.members.update((members) => members.map((entry) => (entry.id === username ? { ...entry, name: role } : entry)));
        this.notify.success('Member role saved');
        if (this.isSignedInUser(username)) this.navigateToCacheOrReloadCache(false);
      });
  }

  removeMember(username: string): void {
    this.confirm
      .confirm({
        title: 'Remove member',
        target: formatMemberNameForConfirmDialog(this.members().find((member) => member.id === username)?.user_display_name, username),
        consequence: getMemberRemovalConsequenceSentence('cache', username, this.isSignedInUser(username)),
        confirmLabel: 'Remove',
        failure: 'Failed to remove member.',
        action: () => this.cachesService.removeMember(this.cacheName, username),
      })
      .subscribe(() => {
        this.notify.success('Member removed');
        if (this.isSignedInUser(username)) {
          this.navigateToCacheOrReloadCache(true);
          return;
        }
        this.loadMembers();
      });
  }

  navigateToCacheOrReloadCache(removed: boolean): void {
    if (removed) {
      void this.router.navigateByUrl(links.cache(this.cacheName));
      return;
    }
    this.cachesService.getCache(this.cacheName).subscribe({
      next: (cache) => this.cache.set(cache),
      error: () => void this.router.navigateByUrl(links.cache(this.cacheName)),
    });
  }

  private isSignedInUser(username: string): boolean {
    return this.auth.user()?.username === username;
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
        consequence: '',
        isIrreversible: true,
        confirmLabel: 'Delete',
        failure: 'Failed to delete role.',
        action: () => {
          this.deletingRole.set(true);
          return this.cachesService.deleteRole(this.cacheName, role.id).pipe(finalize(() => this.deletingRole.set(false)));
        },
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
