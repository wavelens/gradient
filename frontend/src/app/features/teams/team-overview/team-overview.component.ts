/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, signal } from '@angular/core';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { TeamsService } from '@core/services/teams.service';
import { AuthService } from '@core/services/auth.service';
import { apiErrorMessage } from '@core/services/api.service';
import { NotifyService } from '@core/services/notify.service';
import { TeamResponse, TeamGrants, TeamRequestItem } from '@core/models';
import {
  ButtonComponent,
  LoadingSpinnerComponent,
  MessageBannerComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
  SettingsSectionComponent,
} from '@gradient/ui/ui';
import { TeamMembersComponent } from '../team-members/team-members.component';
import { TeamWorkersComponent } from '../team-workers/team-workers.component';

@Component({
  selector: 'app-team-overview',
  standalone: true,
  imports: [
    RouterModule,
    ButtonComponent,
    LoadingSpinnerComponent,
    TeamMembersComponent,
    TeamWorkersComponent,
    MessageBannerComponent,
    PageLayoutComponent,
    RowComponent,
    RowListComponent,
    SettingsSectionComponent,
  ],
  templateUrl: './team-overview.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class TeamOverviewComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private teams = inject(TeamsService);
  private crumbs = inject(BreadcrumbsService);
  private authService = inject(AuthService);
  private notify = inject(NotifyService);

  teamName = '';
  breadcrumb = computed(() => this.crumbs.team(this.teamName));
  team = signal<TeamResponse | null>(null);
  grants = signal<TeamGrants>({ projects: [], caches: [] });
  requests = signal<TeamRequestItem[]>([]);
  loading = signal(true);
  busy = signal<string | null>(null);
  error = signal<string | null>(null);

  isAdmin = computed(() => this.team()?.role === 'admin' || this.authService.user()?.superuser === true);
  ngOnInit(): void {
    this.teamName = this.route.snapshot.paramMap.get('team') || '';
    this.teams.get(this.teamName).subscribe({
      next: (team) => {
        this.team.set(team);
        this.crumbs.rememberTeam(this.teamName, team.display_name);
        this.loading.set(false);
        if (this.isAdmin()) this.loadRequests();
      },
      error: (err: unknown) => {
        this.error.set(apiErrorMessage(err, 'Failed to load the team.'));
        this.loading.set(false);
      },
    });
    this.loadGrants();
  }

  grantMeta(grant: { role?: string | null; users: boolean; workers: boolean }): string {
    const parts: string[] = [];
    if (grant.users) parts.push(`Users as ${grant.role ?? 'unknown role'}`);
    if (grant.workers) parts.push('Workers');
    return parts.join(' · ');
  }

  requestMeta(request: TeamRequestItem): string {
    return [request.kind, request.target, this.grantMeta(request)].filter((part) => part).join(' · ');
  }

  approve(id: string): void {
    this.decide(id, this.teams.approveRequest(this.teamName, id), 'Request approved', 'Failed to approve request');
  }

  deny(id: string): void {
    this.decide(id, this.teams.denyRequest(this.teamName, id), 'Request denied', 'Failed to deny request');
  }

  private decide(id: string, call: ReturnType<TeamsService['approveRequest']>, done: string, failed: string): void {
    this.busy.set(id);
    call.subscribe({
      next: () => {
        this.busy.set(null);
        this.notify.success(done);
        this.loadRequests();
        this.loadGrants();
      },
      error: (err: unknown) => {
        this.busy.set(null);
        this.notify.failure(failed, err);
      },
    });
  }

  private loadGrants(): void {
    this.teams.grants(this.teamName).subscribe({ next: (grants) => this.grants.set(grants) });
  }

  private loadRequests(): void {
    this.teams.requests(this.teamName).subscribe({
      next: (requests) => this.requests.set(requests),
      error: () => this.requests.set([]),
    });
  }
}
