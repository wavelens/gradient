/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, signal } from '@angular/core';
import { DashboardStatsComponent } from './stats/dashboard-stats.component';
import { DashboardTaskTableComponent } from './task-table/dashboard-task-table.component';
import { DashboardActivityComponent } from './activity/dashboard-activity.component';
import { DashboardRailComponent } from './rail/dashboard-rail.component';
import { DashboardStartComponent } from './start/dashboard-start.component';

@Component({
  selector: 'app-dashboard',
  standalone: true,
  imports: [
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
}
