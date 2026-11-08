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
import { canManageTeam } from '@shared/access';
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

  readonly roles: { label: string; value: TeamRole }[] = [
    { label: 'Admin', value: 'admin' },
    { label: 'Member', value: 'member' },
  ];

  team = input.required<TeamResponse>();
  currentUserTeamAccessChanged = output<boolean>();
  loading = signal(true);
  loadError = signal<unknown>(null);
  members = signal<TeamMemberItem[]>([]);
  invitations = signal<PendingInvitationItem[]>([]);
  suggestions = signal<string[]>([]);
  addError = signal<string | null>(null);
  busy = signal<string | null>(null);
  pendingMemberRoleSelection = signal<{ user: string; role: TeamRole } | null>(null);
  showAdd = signal(false);
  addAttempted = signal(false);
  newMember: { user: string; role: TeamRole } = { user: '', role: 'member' };

  readonly sourceLabels: Record<TeamMemberSource, string | null> = { api: null, state: 'State', group: 'Group' };

  canEdit = computed(() => canManageTeam(this.team(), this.auth.user()));
  countLabel = computed(() => `${this.members().length} ${this.members().length === 1 ? 'member' : 'members'}`);

  private get teamName(): string {
    return this.team().name;
  }

  private isSignedInUser(user: string): boolean {
    return this.auth.user()?.username === user;
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
      next: (found) => this.suggestions.set(found.map((u) => u.username)),
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
      next: () => {
        this.busy.set(null);
        this.showAdd.set(false);
        this.notify.success('Member added');
        this.load();
      },
      error: (err: unknown) => {
        this.busy.set(null);
        this.addError.set(apiErrorMessage(err, 'Failed to add the member.'));
      },
    });
  }

  updateRole(user: string, role: TeamRole): void {
    this.pendingMemberRoleSelection.set({ user, role });
    this.teams.updateMember(this.teamName, user, role).subscribe({
      next: () => {
        this.members.update((members) => members.map((member) => (member.user === user ? { ...member, role } : member)));
        this.pendingMemberRoleSelection.set(null);
        if (this.isSignedInUser(user)) this.currentUserTeamAccessChanged.emit(false);
      },
      error: (err: unknown) => {
        this.pendingMemberRoleSelection.set(null);
        this.notify.failure('Role change failed', err);
      },
    });
  }

  removeMember(user: string): void {
    const self = this.isSignedInUser(user);
    this.confirm
      .confirm({
        title: 'Remove member',
        target: user,
        consequence: self ? 'You lose access to this team and its settings.' : `${user} can no longer access this team.`,
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
        target: user,
        consequence: `The invitation to ${user} becomes invalid. You can invite them again later.`,
        confirmLabel: 'Revoke',
        failure: 'Failed to revoke the invitation.',
        action: () => this.teams.revokeInvitation(this.teamName, user),
      })
      .subscribe(() => {
        this.notify.success('Invitation revoked');
        this.load();
      });
  }
}
