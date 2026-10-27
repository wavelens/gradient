/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, input, signal } from '@angular/core';
import { DatePipe } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { TeamsService } from '@core/services/teams.service';
import { UserService } from '@core/services/user.service';
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
  EmptyStateComponent,
  FormFieldComponent,
  LoadingSpinnerComponent,
  MessageBannerComponent,
  RowComponent,
  RowListComponent,
  SelectComponent,
  SettingsSectionComponent,
} from '@gradient/ui/ui';

@Component({
  selector: 'app-team-members',
  standalone: true,
  imports: [
    DatePipe,
    FormsModule,
    AutoCompleteComponent,
    BadgeComponent,
    ButtonComponent,
    DialogComponent,
    EmptyStateComponent,
    FormFieldComponent,
    LoadingSpinnerComponent,
    MessageBannerComponent,
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

  readonly roles: { label: string; value: TeamRole }[] = [
    { label: 'Admin', value: 'admin' },
    { label: 'Member', value: 'member' },
  ];

  team = input.required<TeamResponse>();
  loading = signal(true);
  members = signal<TeamMemberItem[]>([]);
  invitations = signal<PendingInvitationItem[]>([]);
  suggestions = signal<string[]>([]);
  error = signal<string | null>(null);
  addError = signal<string | null>(null);
  busy = signal<string | null>(null);
  showAdd = signal(false);
  newMember: { user: string; role: TeamRole } = { user: '', role: 'member' };

  readonly sourceLabels: Record<TeamMemberSource, string | null> = { api: null, state: 'State', group: 'Group' };

  canEdit = computed(() => this.team().role === 'admin');
  countLabel = computed(() => `${this.members().length} ${this.members().length === 1 ? 'member' : 'members'}`);

  private get teamName(): string {
    return this.team().name;
  }

  ngOnInit(): void {
    this.load();
  }

  load(): void {
    this.teams.members(this.teamName).subscribe({
      next: (members) => {
        this.members.set(members);
        this.loading.set(false);
      },
      error: () => this.loading.set(false),
    });
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
    this.addError.set(null);
    this.showAdd.set(true);
  }

  addMember(): void {
    if (!this.newMember.user) return;
    this.busy.set('add');
    this.addError.set(null);
    this.teams.addMember(this.teamName, this.newMember.user, this.newMember.role).subscribe({
      next: () => {
        this.busy.set(null);
        this.showAdd.set(false);
        this.load();
      },
      error: (err: Error) => {
        this.busy.set(null);
        this.addError.set(err.message || 'Failed to add the member.');
      },
    });
  }

  updateRole(user: string, role: TeamRole): void {
    this.busy.set(user);
    this.teams.updateMember(this.teamName, user, role).subscribe({
      next: () => {
        this.busy.set(null);
        this.load();
      },
      error: (err: Error) => {
        this.fail(err, 'Failed to change the role.');
        this.load();
      },
    });
  }

  removeMember(user: string): void {
    this.busy.set(user);
    this.teams.removeMember(this.teamName, user).subscribe({
      next: () => {
        this.busy.set(null);
        this.load();
      },
      error: (err: Error) => this.fail(err, 'Failed to remove the member.'),
    });
  }

  revokeInvitation(user: string): void {
    this.busy.set(user);
    this.teams.revokeInvitation(this.teamName, user).subscribe({
      next: () => {
        this.busy.set(null);
        this.load();
      },
      error: (err: Error) => this.fail(err, 'Failed to revoke the invitation.'),
    });
  }

  private fail(err: Error, fallback: string): void {
    this.busy.set(null);
    this.error.set(err.message || fallback);
  }
}
