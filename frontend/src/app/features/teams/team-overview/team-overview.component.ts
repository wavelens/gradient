/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, linkedSignal, signal } from '@angular/core';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { TeamsService } from '@core/services/teams.service';
import { AuthService } from '@core/services/auth.service';
import { injectTeam } from '@core/resolvers/team-access.resolver';
import { NotifyService } from '@core/services/notify.service';
import { TeamResponse, TeamGrants, TeamRequestItem } from '@core/models';
import { ConfirmService } from '@shared/confirm';
import { canManageTeam } from '@shared/access';
import {
  ButtonComponent,
  LoadStateComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
  SettingsSectionComponent,
} from '@gradient/ui/ui';
import { TeamMembersComponent } from '../team-members/team-members.component';
import { TeamWorkersComponent } from '../team-workers/team-workers.component';
import { links } from '@shared/routing';
import { EMPTY, merge, tap } from 'rxjs';
import { runReloadIntervalWhileTabVisible } from '@shared/paging';

@Component({
  selector: 'app-team-overview',
  standalone: true,
  imports: [
    RouterModule,
    ButtonComponent,
    LoadStateComponent,
    TeamMembersComponent,
    TeamWorkersComponent,
    PageLayoutComponent,
    RowComponent,
    RowListComponent,
    SettingsSectionComponent,
  ],
  templateUrl: './team-overview.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class TeamOverviewComponent implements OnInit {
  protected readonly links = links;
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private teams = inject(TeamsService);
  private crumbs = inject(BreadcrumbsService);
  private authService = inject(AuthService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);

  private resolvedTeam = injectTeam();

  teamName = '';
  breadcrumb = computed(() => this.crumbs.team(this.teamName));
  team = linkedSignal<TeamResponse | null>(() => this.resolvedTeam() ?? null);
  grants = signal<TeamGrants>({ projects: [], caches: [] });
  grantsLoading = signal(true);
  grantsLoadError = signal<unknown>(null);
  requests = signal<TeamRequestItem[]>([]);
  busy = signal<string | null>(null);
  teamWorkerCount = signal<number | null>(null);

  isAdmin = computed(() => canManageTeam(this.team(), this.authService.user()));
  isTeamAccessEmptyAndGrantsLoadSucceeded = computed(() => {
    const grants = this.grants();
    return !this.grantsLoading() && this.grantsLoadError() === null && grants.projects.length + grants.caches.length === 0 && this.teamWorkerCount() === 0;
  });
  constructor() {
    runReloadIntervalWhileTabVisible(() => merge(
      this.grantsLoadError() !== null ? EMPTY : this.teams.grants(this.teamName).pipe(tap((grants) => this.grants.set(grants))),
      this.isAdmin() ? this.teams.requests(this.teamName).pipe(tap((requests) => this.requests.set(requests))) : EMPTY,
    ));
  }

  ngOnInit(): void {
    this.teamName = this.route.snapshot.paramMap.get('team') || '';
    if (this.isAdmin()) this.loadRequests();
    this.loadGrants();
  }

  grantMeta(grant: { role?: string | null; users: boolean; workers: boolean }): string {
    const parts: string[] = [];
    if (grant.users) parts.push(`Members get ${grant.role ?? 'unknown role'}`);
    if (grant.workers) parts.push('Workers');
    return parts.join(' · ');
  }

  requestMeta(request: TeamRequestItem): string {
    return [request.kind, request.target, this.grantMeta(request)].filter((part) => part).join(' · ');
  }

  approve(id: string): void {
    this.busy.set(id);
    this.teams.approveRequest(this.teamName, id).subscribe({
      next: () => {
        this.busy.set(null);
        this.decided('Request approved');
      },
      error: (err: unknown) => {
        this.busy.set(null);
        this.notify.failure('Failed to approve request', err);
      },
    });
  }

  deny(request: TeamRequestItem): void {
    this.confirm
      .confirm({
        title: 'Deny Request',
        target: request.display_name,
        consequence: 'The request is deleted and nothing is granted.',
        confirmLabel: 'Deny',
        failure: 'Failed to deny request.',
        action: () => this.teams.denyRequest(this.teamName, request.id),
      })
      .subscribe(() => this.decided('Request denied'));
  }

  navigateToTeamListOrReloadTeam(removed: boolean): void {
    if (removed && !this.authService.user()?.superuser) {
      void this.router.navigateByUrl(links.teams());
      return;
    }
    this.teams.get(this.teamName).subscribe({
      next: (team) => this.team.set(team),
      error: () => void this.router.navigateByUrl(links.teams()),
    });
  }

  private decided(done: string): void {
    this.notify.success(done);
    this.loadRequests();
    this.loadGrants();
  }

  loadGrants(): void {
    const grants = this.grants();
    this.grantsLoadError.set(null);
    this.grantsLoading.set(grants.projects.length + grants.caches.length === 0);
    this.teams.grants(this.teamName).subscribe({
      next: (loaded) => {
        this.grants.set(loaded);
        this.grantsLoading.set(false);
      },
      error: (error: unknown) => {
        this.grantsLoadError.set(error);
        this.grantsLoading.set(false);
      },
    });
  }

  private loadRequests(): void {
    this.teams.requests(this.teamName).subscribe({
      next: (requests) => this.requests.set(requests),
      error: () => this.requests.set([]),
    });
  }
}
