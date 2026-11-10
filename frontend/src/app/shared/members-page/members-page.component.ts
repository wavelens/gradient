/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, linkedSignal, signal } from '@angular/core';
import { ActivatedRoute, Router } from '@angular/router';
import { EMPTY, Observable, finalize, tap } from 'rxjs';
import {
  ButtonComponent,
  LinkTabsComponent,
  LoadStateComponent,
  PageLayoutComponent,
  SettingsSectionComponent,
  type LinkTab,
} from '@gradient/ui/ui';
import { accessTo, type AccessState, type PermissionEntry, type TeamRole } from '@core/models';
import { AuthService } from '@core/services/auth.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { NotifyService } from '@core/services/notify.service';
import { ProjectsService } from '@core/services/projects.service';
import { CachesService } from '@core/services/caches.service';
import { TeamsService } from '@core/services/teams.service';
import { injectProject } from '@core/resolvers/project-access.resolver';
import { injectTeam } from '@core/resolvers/team-access.resolver';
import { injectCacheAccessData } from '@core/resolvers/inject-access';
import {
  ManagedDisableDirective,
  ServerConfigurationManagedBannerComponent,
  WritableDirective,
  buildMemberSoleAdminDisabledReasonTextForUsername,
  canManageTeam,
  compareRoleNamesByBuiltInAccessThenName,
  createNoAccessLoadStateError,
  formatMemberNameForConfirmDialog,
  getMemberRemovalConsequenceSentence,
  injectSaveMemberRoleChangeWithOwnRoleConfirmation,
  isViewerCacheMember,
  isViewerProjectMember,
  newRoleLacksCurrentRolePermission,
  requestIfCacheMember,
  requestIfProjectMember,
  teamRoleDisplayLabels,
  type MemberPlace,
} from '@shared/access';
import { ConfirmService } from '@shared/confirm';
import { runReloadIntervalWhileTabVisible } from '@shared/paging';
import { links, type MembersPageTab } from '@shared/routing';
import { formatEntityLabel } from '@shared/ui';
import { TeamGrantsComponent } from '@features/teams/team-grants/team-grants.component';
import {
  type PeopleListRow,
  copyPeopleRowAfterRoleSave,
  getRemoveMemberDialogSentence,
  getTeamsProvidingAccessAfterDirectRoleRemoval,
} from './access-reason-label';
import {
  type ProjectOrCacheRole,
  buildMemberRequestForKindAndName,
  buildRoleListCreateEditDeleteRequestsForKindAndName,
} from './members-page-requests';
import { ProjectMembersAccessListComponent } from './project-members-access-list.component';
import { InvitePersonDialogComponent } from './invite-person-dialog.component';
import { ProjectOrCacheRolesListComponent } from './project-or-cache-roles-list.component';
import { PendingInvitationsAndTeamGrantRequestsComponent } from './pending-invitations-and-team-grant-requests.component';

const teamMemberRoleOptions = (Object.keys(teamRoleDisplayLabels) as TeamRole[]).map((value) => ({ label: teamRoleDisplayLabels[value], value }));
const roleNamesForUnavailableRoleList = ['View', 'Write', 'Admin'];

@Component({
  selector: 'app-members-page',
  standalone: true,
  imports: [
    ButtonComponent,
    LinkTabsComponent,
    LoadStateComponent,
    PageLayoutComponent,
    SettingsSectionComponent,
    ManagedDisableDirective,
    ServerConfigurationManagedBannerComponent,
    WritableDirective,
    TeamGrantsComponent,
    ProjectMembersAccessListComponent,
    InvitePersonDialogComponent,
    ProjectOrCacheRolesListComponent,
    PendingInvitationsAndTeamGrantRequestsComponent,
  ],
  templateUrl: './members-page.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class MembersPageComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private auth = inject(AuthService);
  private crumbs = inject(BreadcrumbsService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);
  private projects = inject(ProjectsService);
  private caches = inject(CachesService);
  private teams = inject(TeamsService);
  private saveMemberRoleChange = injectSaveMemberRoleChangeWithOwnRoleConfirmation();

  private params = this.route.snapshot.paramMap;
  readonly kind: MemberPlace = this.params.has('project') ? 'project' : this.params.has('cache') ? 'cache' : 'team';
  readonly name = this.params.get(this.kind) ?? '';
  readonly tab = (this.route.snapshot.data['membersPageTab'] as MembersPageTab | undefined) ?? 'people';
  readonly requests = buildMemberRequestForKindAndName(this.kind, this.name);
  readonly roleRequests = this.kind === 'team' ? null : buildRoleListCreateEditDeleteRequestsForKindAndName(this.kind, this.name);

  private resolvedProject = injectProject();
  private cacheAccess = injectCacheAccessData();
  private resolvedTeam = injectTeam();
  project = linkedSignal(() => this.resolvedProject() ?? null);
  cache = linkedSignal(() => this.cacheAccess()?.cache ?? null);
  team = linkedSignal(() => this.resolvedTeam() ?? null);

  access = computed<AccessState>(() => {
    if (this.kind === 'project') return accessTo(this.project(), 'manageMembers');
    if (this.kind === 'cache') return accessTo(this.cache(), 'manageCacheMembers');
    const canEdit = canManageTeam(this.team(), this.auth.user());
    return { managed: false, canEdit, canTrigger: canEdit };
  });
  roleAccess = computed<AccessState>(() =>
    this.kind === 'project' ? accessTo(this.project(), 'manageRoles') : accessTo(this.cache(), 'manageCacheRoles'),
  );
  isViewerMember = computed(() => {
    if (this.kind === 'project') return isViewerProjectMember(this.project());
    if (this.kind === 'cache') return isViewerCacheMember(this.cache());
    return this.team()?.role != null;
  });
  shouldShowNoAccessStateOnCachePage = computed(
    () => this.kind === 'cache' && !isViewerCacheMember(this.cache()) && this.auth.user()?.superuser !== true,
  );
  tabContentNoAccessError = computed(() => {
    if (this.shouldShowNoAccessStateOnCachePage()) return createNoAccessLoadStateError('cache', 'members');
    if (this.tab === 'pending' && !this.access().canEdit) return createNoAccessLoadStateError(this.kind, 'admins');
    if (this.tab === 'roles' && !this.isViewerMember()) return createNoAccessLoadStateError(this.kind, 'members');
    return null;
  });

  breadcrumb = computed(() => {
    const crumb = { label: 'Members' };
    if (this.kind === 'project') return this.crumbs.project(this.name, crumb);
    if (this.kind === 'cache') return this.crumbs.cache(this.name, crumb);
    return this.crumbs.team(this.name, crumb);
  });
  tabs = computed<LinkTab[]>(() => {
    const tab = (label: string, target: MembersPageTab): LinkTab => ({ label, link: links.membersPage(this.kind, this.name, target) });
    const projectOrCache = this.kind !== 'team';
    return [
      tab('People', 'people'),
      ...(projectOrCache ? [tab('Teams', 'teams')] : []),
      ...(this.access().canEdit ? [tab('Pending', 'pending')] : []),
      ...(projectOrCache && this.isViewerMember() ? [tab('Roles', 'roles')] : []),
    ];
  });

  rows = signal<PeopleListRow[]>([]);
  peopleLoading = signal(true);
  peopleLoadError = signal<unknown>(null);
  pendingMemberRoleSelection = signal<{ user: string; role: string } | null>(null);
  showAddMemberDialog = signal(false);

  roles = signal<ProjectOrCacheRole[]>([]);
  availablePermissions = signal<PermissionEntry[]>([]);
  rolesLoading = signal(this.kind !== 'team');
  rolesLoadError = signal<unknown>(null);
  roleNames = computed(() => this.roles().map((role) => role.name));
  roleOptions = computed(() => {
    if (this.kind === 'team') return teamMemberRoleOptions;
    const names = this.roles().length ? this.roleNames() : roleNamesForUnavailableRoleList;
    return names.map((name) => ({ label: name, value: name }));
  });

  memberOnlyAdminDisabledReason = computed(() => {
    const admin = this.kind === 'team' ? 'admin' : 'Admin';
    return buildMemberSoleAdminDisabledReasonTextForUsername(
      this.kind,
      this.rows().filter((row) => row.directRole === admin).map((row) => row.user),
    );
  });
  teamGrantUsernamesByTeamName = computed(() => {
    const byTeam: Record<string, string[]> = {};
    for (const row of this.rows()) {
      for (const reason of row.reasons) {
        if (reason.kind === 'team' && reason.team) (byTeam[reason.team.name] ??= []).push(row.user);
      }
    }
    return byTeam;
  });

  constructor() {
    runReloadIntervalWhileTabVisible(() =>
      this.tabContentNoAccessError() || this.peopleLoading() || this.peopleLoadError() !== null
        ? EMPTY
        : this.requests.people().pipe(tap((rows) => this.rows.set(rows))),
    );
  }

  ngOnInit(): void {
    if (this.shouldShowNoAccessStateOnCachePage()) return;
    this.loadMembers();
    this.loadRoles();
  }

  loadMembers(): void {
    this.peopleLoading.set(true);
    this.peopleLoadError.set(null);
    this.requests.people().subscribe({
      next: (rows) => {
        this.rows.set(rows);
        this.peopleLoading.set(false);
      },
      error: (error: unknown) => {
        this.peopleLoadError.set(error);
        this.peopleLoading.set(false);
      },
    });
  }

  reloadPeopleRowsInBackgroundKeepingOldRowsOnFailure(): void {
    this.requests.people().subscribe({ next: (rows) => this.rows.set(rows), error: () => undefined });
  }

  loadRoles(): void {
    const requests = this.roleRequests;
    if (!requests) return;
    this.rolesLoading.set(true);
    this.rolesLoadError.set(null);
    const roles = this.kind === 'project'
      ? requestIfProjectMember(this.project(), () => requests.roles())
      : requestIfCacheMember(this.cache(), () => requests.roles());
    roles.subscribe({
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

  updateMemberRole({ user, role }: { user: string; role: string }): void {
    const row = this.rows().find((entry) => entry.user === user);
    const current = row?.directRole ?? row?.role ?? '';
    if (!row || role === current) return;
    const signedInUser = this.isSignedInUser(user);
    const lowersAccess = this.kind === 'team'
      ? current === 'admin' && role !== 'admin'
      : newRoleLacksCurrentRolePermission(this.roles(), current, role);
    this.pendingMemberRoleSelection.set({ user, role });
    this.saveMemberRoleChange({
      place: this.kind,
      signedInUser,
      lowersAccess,
      target: formatMemberNameForConfirmDialog(row.displayName, user),
      save: () => (row.directRole ? this.requests.changeRole(user, role) : this.requests.addMember(user, role, true)),
    })
      .pipe(finalize(() => this.pendingMemberRoleSelection.set(null)))
      .subscribe(() => {
        this.rows.update((rows) => rows.map((entry) => (entry.user === user ? copyPeopleRowAfterRoleSave(entry, role) : entry)));
        this.notify.success('Member role saved');
        if (signedInUser) this.reloadResolvedEntityAfterOwnMembershipChangeOrReturnToListOnFailure();
        if (this.kind !== 'team') this.reloadPeopleRowsInBackgroundKeepingOldRowsOnFailure();
      });
  }

  removeMember(user: string): void {
    const row = this.rows().find((entry) => entry.user === user);
    if (!row) return;
    const signedInUser = this.isSignedInUser(user);
    const [keepingTeam] = this.kind === 'team' ? [] : getTeamsProvidingAccessAfterDirectRoleRemoval(row);
    this.confirm
      .confirm({
        title: 'Remove member',
        target: formatMemberNameForConfirmDialog(row.displayName, user),
        consequence: this.kind === 'team'
          ? getMemberRemovalConsequenceSentence('team', user, signedInUser)
          : getRemoveMemberDialogSentence(this.kind, row, signedInUser),
        consequenceLink: keepingTeam
          ? { label: `Open ${formatEntityLabel(keepingTeam)}`, link: links.team(keepingTeam.name) }
          : undefined,
        confirmLabel: 'Remove',
        failure: "Couldn't delete member",
        action: () => this.requests.removeMember(user),
      })
      .subscribe(() => {
        this.notify.success('Member deleted');
        if (row.reasons.some((reason) => reason.kind !== 'direct')) this.reloadPeopleRowsInBackgroundKeepingOldRowsOnFailure();
        else this.rows.update((rows) => rows.filter((entry) => entry.user !== user));
        if (signedInUser) this.reloadResolvedEntityAfterOwnMembershipChangeOrReturnToListOnFailure();
      });
  }

  reloadResolvedEntityAfterOwnMembershipChangeOrReturnToListOnFailure(): void {
    const reload: Observable<unknown> =
      this.kind === 'project'
        ? this.projects.getProject(this.name).pipe(tap((project) => this.project.set(project)))
        : this.kind === 'cache'
          ? this.caches.getCache(this.name).pipe(tap((cache) => this.cache.set(cache)))
          : this.teams.get(this.name).pipe(tap((team) => this.team.set(team)));
    const list = this.kind === 'project' ? links.projects() : this.kind === 'cache' ? links.caches() : links.teams();
    reload.subscribe({ error: () => void this.router.navigateByUrl(list) });
  }

  private isSignedInUser(user: string): boolean {
    return this.auth.user()?.username === user;
  }
}
