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
import { RouterModule, type UrlTree } from '@angular/router';
import { Observable } from 'rxjs';
import { TeamsService } from '@core/services/teams.service';
import { AuthService } from '@core/services/auth.service';
import { apiErrorMessage } from '@core/services/api.service';
import { NotifyService } from '@core/services/notify.service';
import type { CacheResponse, TeamGrantEntry, TeamSummary } from '@core/models';
import { canOpenTeam, requestIfCacheMember } from '@shared/access';
import { ConfirmService } from '@shared/confirm';
import { CreateTeamDialogComponent, injectCanCreateTeam } from '@shared/forms';
import {
  AutoCompleteComponent,
  BadgeComponent,
  ButtonComponent,
  DialogComponent,
  LoadStateComponent,
  FormFieldComponent,
  RowComponent,
  RowListComponent,
  SelectComponent,
  SettingsSectionComponent,
} from '@gradient/ui/ui';
import { links } from '@shared/routing';

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
    LoadStateComponent,
    FormFieldComponent,
    RowComponent,
    RowListComponent,
    SelectComponent,
    SettingsSectionComponent,
    CreateTeamDialogComponent,
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
  cacheWithViewerPermissions = input<Pick<CacheResponse, 'permissions'> | null>();
  grants = input<GrantedPart>('users');
  roles = input<string[]>([]);
  canEdit = input(false);
  changed = output<void>();
  teamGrantsWithMemberRoles = output<TeamGrantEntry[]>();

  part = computed<GrantedPart>(() => (this.kind() === 'cache' ? 'users' : this.grants()));
  roleOptions = computed(() => this.roles().map((role) => ({ label: role, value: role })));
  allGrants = signal<TeamGrantEntry[]>([]);
  loading = signal(true);
  loadError = signal<unknown>(null);
  shownGrants = computed(() => this.allGrants().filter((grant) => grant[this.part()]));
  myTeams = signal<TeamSummary[]>([]);
  suggestions = signal<string[]>([]);
  grantError = signal<string | null>(null);
  granting = signal(false);
  showGrant = signal(false);
  grantAttempted = signal(false);
  form: GrantForm = { team: '', role: '' };
  showCreateTeam = signal(false);
  private createForGrant = false;
  canCreateTeam = injectCanCreateTeam();

  ngOnInit(): void {
    this.load();
    this.teams.list().subscribe({ next: (mine) => this.myTeams.set(mine) });
  }

  teamLink(team: string): UrlTree | undefined {
    return canOpenTeam(this.authService.user(), this.myTeams(), team) ? links.team(team) : undefined;
  }

  load(): void {
    const grants = this.kind() === 'project'
      ? this.teams.projectGrants(this.name())
      : requestIfCacheMember(this.cacheWithViewerPermissions(), () => this.teams.cacheGrants(this.name()), 'viewCache');
    this.loadError.set(null);
    this.loading.set(this.allGrants().length === 0);
    grants.subscribe({
      next: (items) => {
        this.allGrants.set(items);
        this.teamGrantsWithMemberRoles.emit(items.filter((grant) => grant.users));
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  search(event: { query: string }): void {
    const query = event.query.toLowerCase();
    this.suggestions.set(
      this.myTeams()
        .filter((team) => this.canOfferTeamInGrantPicker(team))
        .filter((team) => `${team.name} ${team.display_name}`.toLowerCase().includes(query))
        .map((team) => team.name),
    );
  }

  private canOfferTeamInGrantPicker(team: TeamSummary): boolean {
    const grant = this.allGrants().find((entry) => entry.team === team.name);
    if (!grant) return true;
    if (grant[this.part()]) return false;
    return this.part() === 'users' || (!grant.pending && team.role === 'admin');
  }

  openGrant(): void {
    this.form = { team: '', role: this.roles()[0] ?? '' };
    this.grantError.set(null);
    this.grantAttempted.set(false);
    this.showGrant.set(true);
  }

  createInsteadOfPick(): void {
    this.createForGrant = true;
    this.showGrant.set(false);
    this.showCreateTeam.set(true);
  }

  teamCreated(team: string): void {
    this.createForGrant = false;
    this.teams.list().subscribe({ next: (mine) => this.myTeams.set(mine) });
    this.form = { ...this.form, team };
    this.showGrant.set(true);
  }

  createTeamClosed(visible: boolean): void {
    if (visible || !this.createForGrant) return;
    this.createForGrant = false;
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
      next: (message) => {
        this.granting.set(false);
        this.showGrant.set(false);
        this.notify.success(message);
        this.refresh();
      },
      error: (err: unknown) => {
        this.granting.set(false);
        this.grantError.set(apiErrorMessage(err, 'Failed to grant to team.'));
      },
    });
  }

  remove(grant: TeamGrantEntry): void {
    const team = grant.display_name;
    const workers = this.part() === 'workers';
    this.confirm
      .confirm({
        title: grant.pending
          ? `Withdraw request for ${team}`
          : workers
            ? `Stop using ${team} workers`
            : `Remove access for ${team}`,
        target: team,
        consequence: this.removalConsequence(grant),
        confirmLabel: grant.pending ? 'Withdraw' : 'Remove',
        failure: "Couldn't remove team access",
        action: () =>
          this.kind() === 'project' ? this.removeFromProject(grant) : this.teams.removeCacheGrant(this.name(), grant.team),
      })
      .subscribe(() => {
        this.notify.success(grant.pending ? 'Request withdrawn' : workers ? 'Team workers removed' : 'Team access removed');
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
