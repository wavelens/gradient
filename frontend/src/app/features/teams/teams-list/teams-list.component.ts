/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, inject, signal } from '@angular/core';
import { Router, RouterModule } from '@angular/router';
import { TeamsService } from '@core/services/teams.service';
import { TeamSummary } from '@core/models';
import {
  BadgeComponent,
  ButtonComponent,
  LoadStateComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
} from '@gradient/ui/ui';
import { CreateTeamDialogComponent, injectCanCreateTeam, openCreateDialogFromUrlIfAllowed } from '@shared/forms';
import { links } from '@shared/routing';
import { PendingInvitationsBannerComponent, teamRoleDisplayLabels } from '@shared/access';

@Component({
  selector: 'app-teams-list',
  standalone: true,
  imports: [
    RouterModule,
    BadgeComponent,
    ButtonComponent,
    LoadStateComponent,
    PageLayoutComponent,
    RowComponent,
    RowListComponent,
    CreateTeamDialogComponent,
    PendingInvitationsBannerComponent,
  ],
  templateUrl: './teams-list.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class TeamsListComponent implements OnInit {
  protected readonly links = links;
  protected readonly teamRoleDisplayLabels = teamRoleDisplayLabels;
  private teams = inject(TeamsService);
  private router = inject(Router);
  protected canCreateTeam = injectCanCreateTeam();

  loading = signal(true);
  loadError = signal<unknown>(null);
  items = signal<TeamSummary[]>([]);
  showCreate = signal(false);

  constructor() {
    openCreateDialogFromUrlIfAllowed(this.canCreateTeam, () => this.openCreate());
  }

  openCreate(): void {
    this.showCreate.set(true);
  }

  ngOnInit(): void {
    this.load();
  }

  load(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.teams.list().subscribe({
      next: (items) => {
        this.items.set(items);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  openTeam(name: string): void {
    this.router.navigateByUrl(links.team(name));
  }
}
