/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, input, output, signal } from '@angular/core';
import { ServerTimestampToLocalDateTimePipe } from '@shared/text';
import { FormsModule } from '@angular/forms';
import { TeamsService } from '@core/services/teams.service';
import { UserService } from '@core/services/user.service';
import { apiErrorMessage } from '@core/services/api.service';
import { NotifyService } from '@core/services/notify.service';
import { AuthService } from '@core/services/auth.service';
import { ConfirmService } from '@shared/confirm';
import { PENDING_INVITATION_REVOKE_FAILURE, getPendingInvitationRevocationConsequenceSentence } from '@shared/access/pending-invitation-revoke-dialog-texts';
import {
  buildMemberSoleAdminDisabledReasonTextForUsername,
  canManageTeam,
  formatMemberNameForConfirmDialog,
  getMemberRemovalConsequenceSentence,
  injectSaveMemberRoleChangeWithOwnRoleConfirmation,
  teamRoleDisplayLabels,
  formatMemberNameForAutocompleteSuggestion,
  teamRoleValueToDisplayLabel,
} from '@shared/access';
import { EMPTY, finalize, merge, tap } from 'rxjs';
import { runReloadIntervalWhileTabVisible } from '@shared/paging';
import {
  TeamRole,
  TeamResponse,
  TeamMemberItem,
  PendingInvitationItem,
  TeamMemberSource,
} from '@core/models';
import {
  AutoCompleteComponent,
  BadgeComponent,
  ButtonComponent,
  DialogComponent,
  FormFieldComponent,
  LoadStateComponent,
  RowComponent,
  RowListComponent,
  SelectComponent,
  SettingsSectionComponent,
  TooltipDirective,
  type AutoCompleteSuggestion,
} from '@gradient/ui/ui';

@Component({
  selector: 'app-team-members',
  standalone: true,
  imports: [
    ServerTimestampToLocalDateTimePipe,
    FormsModule,
    AutoCompleteComponent,
    BadgeComponent,
    ButtonComponent,
    DialogComponent,
    FormFieldComponent,
    LoadStateComponent,
    RowComponent,
    RowListComponent,
    SelectComponent,
    SettingsSectionComponent,
    TooltipDirective,
  ],
  templateUrl: './team-members.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class TeamMembersComponent implements OnInit {
  private teams = inject(TeamsService);
  private users = inject(UserService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);
  private auth = inject(AuthService);
  private saveMemberRoleChange = injectSaveMemberRoleChangeWithOwnRoleConfirmation();

  protected readonly teamRoleDisplayLabels = teamRoleDisplayLabels;
  protected readonly teamRoleValueToDisplayLabel = teamRoleValueToDisplayLabel;
  readonly roles = (Object.keys(teamRoleDisplayLabels) as TeamRole[]).map((value) => ({ label: teamRoleDisplayLabels[value], value }));

  team = input.required<TeamResponse>();
  currentUserTeamAccessChanged = output<boolean>();
  loading = signal(true);
  loadError = signal<unknown>(null);
  members = signal<TeamMemberItem[]>([]);
  invitations = signal<PendingInvitationItem[]>([]);
  suggestions = signal<AutoCompleteSuggestion[]>([]);
  addError = signal<string | null>(null);
  busy = signal<string | null>(null);
  pendingMemberRoleSelection = signal<{ user: string; role: TeamRole } | null>(null);
  showAdd = signal(false);
  addAttempted = signal(false);
  newMember: { user: string; role: TeamRole } = { user: '', role: 'member' };

  readonly sourceLabels: Record<TeamMemberSource, string | null> = { api: null, state: 'Server configuration', group: 'Group' };

  canEdit = computed(() => canManageTeam(this.team(), this.auth.user()));
  memberOnlyAdminDisabledReason = computed(() =>
    buildMemberSoleAdminDisabledReasonTextForUsername(
      'team',
      this.members().filter((member) => member.role === 'admin').map((member) => member.user),
    ),
  );
  readonly allMembersPhraseByMemberSource: Record<TeamMemberSource, string | null> = {
    api: null,
    state: 'from the server configuration',
    group: 'from a group',
  };
  memberSourceSharedByAllMembers = computed<TeamMemberSource | null>(() => {
    const sources = new Set(this.members().map((member) => member.source));
    const [only] = sources;
    return sources.size === 1 && this.sourceLabels[only] ? only : null;
  });
  countLabel = computed(() => {
    const count = `${this.members().length} ${this.members().length === 1 ? 'member' : 'members'}`;
    const shared = this.memberSourceSharedByAllMembers();
    return shared ? `${count} ${this.allMembersPhraseByMemberSource[shared]}` : count;
  });

  private get teamName(): string {
    return this.team().name;
  }

  private isSignedInUser(user: string): boolean {
    return this.auth.user()?.username === user;
  }

  constructor() {
    runReloadIntervalWhileTabVisible(() => this.loading() || this.loadError() !== null ? EMPTY : merge(
      this.teams.members(this.teamName).pipe(tap((members) => this.members.set(members))),
      this.canEdit() ? this.teams.invitations(this.teamName).pipe(tap((invitations) => this.invitations.set(invitations))) : EMPTY,
    ));
  }

  ngOnInit(): void {
    this.load();
  }

  load(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.teams.members(this.teamName).subscribe({
      next: (members) => {
        this.members.set(members);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
    if (!this.canEdit()) return;
    this.teams.invitations(this.teamName).subscribe({
      next: (invitations) => this.invitations.set(invitations),
      error: () => this.invitations.set([]),
    });
  }

  search(event: { query: string }): void {
    if (!event.query.trim()) {
      this.suggestions.set([]);
      return;
    }
    this.users.searchUsers(event.query).subscribe({
      next: (found) => {
        const memberUsernames = new Set(this.members().map((member) => member.user));
        this.suggestions.set(found.filter((user) => !memberUsernames.has(user.username)).map(formatMemberNameForAutocompleteSuggestion));
      },
      error: () => this.suggestions.set([]),
    });
  }

  openAdd(): void {
    this.newMember = { user: '', role: 'member' };
    this.addError.set(null);
    this.addAttempted.set(false);
    this.showAdd.set(true);
  }

  userError(): string | null {
    return this.addAttempted() && !this.newMember.user.trim() ? 'Enter a username.' : null;
  }

  addMember(): void {
    if (this.busy() === 'add') return;
    this.addAttempted.set(true);
    if (this.userError()) return;
    this.busy.set('add');
    this.addError.set(null);
    this.teams.addMember(this.teamName, this.newMember.user, this.newMember.role).subscribe({
      next: (outcome) => {
        this.busy.set(null);
        this.showAdd.set(false);
        this.notify.success(outcome || 'Member added');
        this.load();
      },
      error: (err: unknown) => {
        this.busy.set(null);
        this.addError.set(apiErrorMessage(err, 'Failed to add the member.'));
      },
    });
  }

  updateRole(user: string, role: TeamRole): void {
    const member = this.members().find((entry) => entry.user === user);
    this.pendingMemberRoleSelection.set({ user, role });
    this.saveMemberRoleChange({
      place: 'team',
      signedInUser: this.isSignedInUser(user),
      lowersAccess: member?.role === 'admin' && role !== 'admin',
      target: formatMemberNameForConfirmDialog(member?.name, user),
      save: () => this.teams.updateMember(this.teamName, user, role),
    })
      .pipe(finalize(() => this.pendingMemberRoleSelection.set(null)))
      .subscribe(() => {
        this.members.update((members) => members.map((entry) => (entry.user === user ? { ...entry, role } : entry)));
        this.notify.success('Member role saved');
        if (this.isSignedInUser(user)) this.currentUserTeamAccessChanged.emit(false);
      });
  }

  removeMember(user: string): void {
    const self = this.isSignedInUser(user);
    this.confirm
      .confirm({
        title: 'Remove member',
        target: formatMemberNameForConfirmDialog(this.members().find((member) => member.user === user)?.name, user),
        consequence: getMemberRemovalConsequenceSentence('team', user, self),
        confirmLabel: 'Remove',
        failure: 'Failed to remove the member.',
        action: () => this.teams.removeMember(this.teamName, user),
      })
      .subscribe(() => {
        this.notify.success('Member removed');
        if (self) this.currentUserTeamAccessChanged.emit(true);
        this.load();
      });
  }

  revokeInvitation(user: string): void {
    this.confirm
      .confirm({
        title: 'Revoke invitation',
        target: formatMemberNameForConfirmDialog(this.invitations().find((invitation) => invitation.user === user)?.name, user),
        consequence: getPendingInvitationRevocationConsequenceSentence(user),
        confirmLabel: 'Revoke',
        failure: PENDING_INVITATION_REVOKE_FAILURE,
        action: () => this.teams.revokeInvitation(this.teamName, user),
      })
      .subscribe(() => {
        this.notify.success('Invitation revoked');
        this.load();
      });
  }
}
