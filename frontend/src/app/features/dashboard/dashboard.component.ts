/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, signal, viewChild } from '@angular/core';
import { merge } from 'rxjs';
import { runReloadIntervalWhileTabVisible } from '@shared/paging';
import { ButtonComponent, MessageBannerComponent } from '@gradient/ui/ui';
import { PendingInvitationsBannerComponent } from '@shared/access';
import { DashboardStatsComponent } from './stats/dashboard-stats.component';
import { DashboardTaskTableComponent } from './task-table/dashboard-task-table.component';
import { DashboardActivityComponent } from './activity/dashboard-activity.component';
import { DashboardRailComponent } from './rail/dashboard-rail.component';
import { DashboardStartComponent } from './start/dashboard-start.component';

@Component({
  selector: 'app-dashboard',
  standalone: true,
  imports: [
    ButtonComponent,
    MessageBannerComponent,
    PendingInvitationsBannerComponent,
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
  // null until the rail answers; the router blocks load alongside it and give way if it reports first steps.
  newUser = signal<boolean | null>(null);

  private stats = viewChild(DashboardStatsComponent);
  private tasks = viewChild(DashboardTaskTableComponent);
  private activity = viewChild(DashboardActivityComponent);
  private rail = viewChild(DashboardRailComponent);
  private reloadableRenderedDashboardSections = computed(() =>
    [this.stats(), this.tasks(), this.rail()].filter((section) => section !== undefined),
  );
  private dashboardSectionComponentsWithErrorBannerNames = computed(() => [
    { component: this.stats(), name: 'statistics' },
    { component: this.activity(), name: 'activity' },
    { component: this.rail(), name: 'projects and caches' },
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
