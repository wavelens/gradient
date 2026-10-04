/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, signal } from '@angular/core';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { TeamsService } from '@core/services/teams.service';
import { AuthService } from '@core/services/auth.service';
import { Team, TeamGrants, TeamMember, TeamRequest, TeamWorker } from '@core/models';
import {
  BadgeComponent,
  ButtonComponent,
  EmptyStateComponent,
  LoadingSpinnerComponent,
  MessageBannerComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
  SettingsSectionComponent,
} from '@gradient/ui/ui';

@Component({
  selector: 'app-team-overview',
  standalone: true,
  imports: [
    RouterModule,
    BadgeComponent,
    ButtonComponent,
    EmptyStateComponent,
    LoadingSpinnerComponent,
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
  private authService = inject(AuthService);

  teamName = '';
  team = signal<Team | null>(null);
  members = signal<TeamMember[]>([]);
  workers = signal<TeamWorker[]>([]);
  grants = signal<TeamGrants>({ projects: [], caches: [] });
  requests = signal<TeamRequest[]>([]);
  loading = signal(true);
  busy = signal<string | null>(null);
  error = signal<string | null>(null);

  isAdmin = computed(() => this.team()?.role === 'admin' || this.authService.user()?.superuser === true);
  firstMembers = computed(() => this.members().slice(0, 5));

  ngOnInit(): void {
    this.teamName = this.route.snapshot.paramMap.get('team') || '';
    this.teams.get(this.teamName).subscribe({
      next: (team) => {
        this.team.set(team);
        this.loading.set(false);
        if (this.isAdmin()) this.loadRequests();
      },
      error: (err: Error) => {
        this.error.set(err.message || 'Failed to load the team.');
        this.loading.set(false);
      },
    });
    this.teams.members(this.teamName).subscribe({ next: (members) => this.members.set(members) });
    this.teams.workers(this.teamName).subscribe({ next: (workers) => this.workers.set(workers) });
    this.loadGrants();
  }

  grantMeta(grant: { role: string | null; users: boolean; workers: boolean }): string {
    const parts: string[] = [];
    if (grant.users) parts.push(`Users as ${grant.role ?? 'unknown role'}`);
    if (grant.workers) parts.push('Workers');
    return parts.join(' · ');
  }

  requestMeta(request: TeamRequest): string {
    return [request.kind, request.target, this.grantMeta(request)].filter((part) => part).join(' · ');
  }

  approve(id: string): void {
    this.decide(id, this.teams.approveRequest(this.teamName, id));
  }

  deny(id: string): void {
    this.decide(id, this.teams.denyRequest(this.teamName, id));
  }

  private decide(id: string, call: ReturnType<TeamsService['approveRequest']>): void {
    this.busy.set(id);
    this.error.set(null);
    call.subscribe({
      next: () => {
        this.busy.set(null);
        this.loadRequests();
        this.loadGrants();
      },
      error: (err: Error) => {
        this.busy.set(null);
        this.error.set(err.message || 'Failed to answer the request.');
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
