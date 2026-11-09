/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, inject, signal, viewChild } from '@angular/core';
import { RouterLink } from '@angular/router';
import { merge } from 'rxjs';
import { runReloadIntervalWhileTabVisible } from '@shared/paging';
import { ButtonComponent, MessageBannerComponent } from '@gradient/ui/ui';
import { PendingInvitesService } from '@core/services/pending-invites.service';
import { links } from '@shared/routing';
import { DashboardStatsComponent } from './stats/dashboard-stats.component';
import { DashboardTaskTableComponent } from './task-table/dashboard-task-table.component';
import { DashboardActivityComponent } from './activity/dashboard-activity.component';
import { DashboardRailComponent } from './rail/dashboard-rail.component';
import { DashboardStartComponent } from './start/dashboard-start.component';

@Component({
  selector: 'app-dashboard',
  standalone: true,
  imports: [
    RouterLink,
    ButtonComponent,
    MessageBannerComponent,
    DashboardStatsComponent,
    DashboardTaskTableComponent,
    DashboardActivityComponent,
    DashboardRailComponent,
    DashboardStartComponent,
  ],
  templateUrl: './dashboard.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './dashboard.component.scss',
})
export class DashboardComponent {
  protected readonly links = links;
  protected invites = inject(PendingInvitesService);
  // null until the rail answers; the router blocks load alongside it and give way if it reports first steps.
  newUser = signal<boolean | null>(null);
  hasAnyProject = signal<boolean | null>(null);

  private stats = viewChild(DashboardStatsComponent);
  private tasks = viewChild(DashboardTaskTableComponent);
  private activity = viewChild(DashboardActivityComponent);
  private rail = viewChild(DashboardRailComponent);
  private reloadableRenderedDashboardSections = computed(() =>
    [this.stats(), this.tasks(), this.rail()].filter((section) => section !== undefined),
  );
  private dashboardSectionComponentsWithErrorBannerNames = computed(() => [
    { component: this.stats(), name: 'Statistics' },
    { component: this.tasks(), name: 'Tasks' },
    { component: this.activity(), name: 'Activity' },
    { component: this.rail(), name: 'Projects and Caches' },
  ]);
  protected failedDashboardSectionComponents = computed(() =>
    this.dashboardSectionComponentsWithErrorBannerNames().filter((section) => section.component?.failed()),
  );
  protected failedSectionNamesForErrorBanner = computed(() =>
    new Intl.ListFormat('en', { type: 'conjunction' }).format(this.failedDashboardSectionComponents().map((section) => section.name)),
  );

  protected retryFailedSections(): void {
    for (const section of this.failedDashboardSectionComponents()) section.component?.load();
  }

  constructor() {
    runReloadIntervalWhileTabVisible(() =>
      merge(...this.reloadableRenderedDashboardSections().map((section) => section.reloadSilentlyKeepingContentOnError())),
    );
  }
}
