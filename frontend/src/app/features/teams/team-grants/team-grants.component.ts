/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  computed,
  inject,
  input,
  output,
  signal,
} from '@angular/core';
import { FormsModule } from '@angular/forms';
import { RouterModule } from '@angular/router';
import { Observable } from 'rxjs';
import { TeamsService } from '@core/services/teams.service';
import { AuthService } from '@core/services/auth.service';
import { apiErrorMessage } from '@core/services/api.service';
import { NotifyService } from '@core/services/notify.service';
import { TeamGrantEntry, TeamSummary } from '@core/models';
import { canOpenTeam } from '@shared/access';
import { ConfirmService } from '@shared/confirm';
import {
  AutoCompleteComponent,
  BadgeComponent,
  ButtonComponent,
  DialogComponent,
  EmptyStateComponent,
  FormFieldComponent,
  RowComponent,
  RowListComponent,
  SelectComponent,
  SettingsSectionComponent,
} from '@gradient/ui/ui';

export type GrantedPart = 'users' | 'workers';

interface GrantForm {
  team: string;
  role: string;
}

@Component({
  selector: 'app-team-grants',
  standalone: true,
  imports: [
    FormsModule,
    RouterModule,
    AutoCompleteComponent,
    BadgeComponent,
    ButtonComponent,
    DialogComponent,
    EmptyStateComponent,
    FormFieldComponent,
    RowComponent,
    RowListComponent,
    SelectComponent,
    SettingsSectionComponent,
  ],
  templateUrl: './team-grants.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class TeamGrantsComponent implements OnInit {
  private teams = inject(TeamsService);
  private authService = inject(AuthService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);

  kind = input.required<'project' | 'cache'>();
  name = input.required<string>();
  grants = input<GrantedPart>('users');
  roles = input<string[]>([]);
  canEdit = input(false);
  changed = output<void>();

  part = computed<GrantedPart>(() => (this.kind() === 'cache' ? 'users' : this.grants()));
  roleOptions = computed(() => this.roles().map((role) => ({ label: role, value: role })));
  allGrants = signal<TeamGrantEntry[]>([]);
  shownGrants = computed(() => this.allGrants().filter((grant) => grant[this.part()]));
  myTeams = signal<TeamSummary[]>([]);
  suggestions = signal<string[]>([]);
  grantError = signal<string | null>(null);
  granting = signal(false);
  showGrant = signal(false);
  grantAttempted = signal(false);
  form: GrantForm = { team: '', role: '' };

  ngOnInit(): void {
    this.load();
    this.teams.list().subscribe({ next: (mine) => this.myTeams.set(mine) });
  }

  teamLink(team: string): string[] | undefined {
    return canOpenTeam(this.authService.user(), this.myTeams(), team) ? ['/team', team] : undefined;
  }

  load(): void {
    const grants = this.kind() === 'project' ? this.teams.projectGrants(this.name()) : this.teams.cacheGrants(this.name());
    grants.subscribe({
      next: (items) => this.allGrants.set(items),
      error: () => this.allGrants.set([]),
    });
  }

  search(event: { query: string }): void {
    const query = event.query.toLowerCase();
    this.suggestions.set(
      this.myTeams()
        .filter((team) => `${team.name} ${team.display_name}`.toLowerCase().includes(query))
        .map((team) => team.name),
    );
  }

  openGrant(): void {
    this.form = { team: '', role: this.roles()[0] ?? '' };
    this.grantError.set(null);
    this.grantAttempted.set(false);
    this.showGrant.set(true);
  }

  teamError(): string | null {
    return this.grantAttempted() && !this.form.team.trim() ? 'Enter a team.' : null;
  }

  grant(): void {
    if (this.granting()) return;
    this.grantAttempted.set(true);
    if (this.teamError()) return;
    const form = { ...this.form, team: this.teamNamed(this.form.team) };
    const request =
      this.kind() === 'project' ? this.grantProject(form) : this.teams.grantCache(this.name(), form.team, form.role);
    this.grantError.set(null);
    this.granting.set(true);
    request.subscribe({
      next: () => {
        this.granting.set(false);
        this.showGrant.set(false);
        this.notify.success('Granted to team');
        this.refresh();
      },
      error: (err: unknown) => {
        this.granting.set(false);
        this.grantError.set(apiErrorMessage(err, 'Failed to grant to team.'));
      },
    });
  }

  remove(grant: TeamGrantEntry): void {
    this.confirm
      .confirm({
        title: 'Remove Team',
        target: grant.display_name,
        consequence: this.removalConsequence(grant),
        confirmLabel: 'Remove',
        failure: 'Could not remove the team.',
        action: () =>
          this.kind() === 'project' ? this.removeFromProject(grant) : this.teams.removeCacheGrant(this.name(), grant.team),
      })
      .subscribe(() => {
        this.notify.success('Removed team');
        this.refresh();
      });
  }

  private removalConsequence(grant: TeamGrantEntry): string {
    const team = grant.display_name;
    const users = this.part() === 'users';
    const otherPart = this.kind() === 'project' && (users ? grant.workers : grant.users);
    if (grant.pending) {
      return otherPart
        ? `The whole pending request for ${team} can no longer get approval, with ${users ? 'workers' : 'member roles'} included.`
        : `The pending request for ${team} can no longer get approval.`;
    }
    if (this.kind() === 'cache') return `Members of ${team} lose their role on this cache.`;
    return users
      ? `Members of ${team} lose their role on this project.`
      : `This project can no longer run builds on ${team} workers.`;
  }

  private teamNamed(typed: string): string {
    const text = typed.trim();
    const mine = this.myTeams();
    const match =
      mine.find((team) => team.name === text) ??
      mine.find((team) => team.display_name.toLowerCase() === text.toLowerCase());
    return match?.name ?? text;
  }

  private grantProject({ team, role }: GrantForm): Observable<string> {
    const existing = this.allGrants().some((grant) => grant.team === team && !grant.pending);
    const users = this.part() === 'users';
    if (existing) {
      return this.teams.updateProjectGrant(this.name(), team, users ? { users: true, role } : { workers: true });
    }
    return this.teams.grantProject(
      this.name(),
      users ? { team, role, users: true, workers: false } : { team, users: false, workers: true },
    );
  }

  private removeFromProject(grant: TeamGrantEntry): Observable<string> {
    const keepsOtherPart = this.part() === 'users' ? grant.workers : grant.users;
    if (keepsOtherPart && !grant.pending) {
      return this.teams.updateProjectGrant(this.name(), grant.team, { [this.part()]: false });
    }
    return this.teams.removeProjectGrant(this.name(), grant.team);
  }

  private refresh(): void {
    this.load();
    this.changed.emit();
  }
}
