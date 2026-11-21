/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal, ChangeDetectionStrategy } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { of } from 'rxjs';
import { ProjectsService } from '@core/services/projects.service';
import { TeamsService } from '@core/services/teams.service';
import {
  BadgeComponent,
  BreadcrumbComponent,
  ButtonComponent,
  CardGridComponent,
  IconComponent,
  Crumb,
  FieldRowComponent,
  FormFieldComponent,
  InputDirective,
  LinkTabsComponent,
  type LinkTab,
  NavCardComponent,
  SelectComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
  SettingsSectionComponent,
  SettingsPageSideNavigationComponent,
  type VerticalNavigationLink,
} from '@gradient/ui/ui';
import { BuildProgressComponent, InputFetchListComponent, ProjectReadinessCardComponent, WaitingNoticeComponent } from '@shared/ui';
import { ServerConfigurationManagedLockButtonComponent } from '@shared/access';
import { waitingAccessFrom } from '@shared/evaluation';
import type { BuildProgress, InputFetch, Permission, ProjectReadinessResponse } from '@core/models';

const sampleProjectReadiness: ProjectReadinessResponse = {
  ready: false,
  workers: {
    ready: false,
    registered: 0,
    teams: [],
    teams_granted_without_workers: [],
    teams_you_can_grant: [{ name: 'build-farm', display_name: 'Build Farm', workers: 4 }],
  },
  cache: { ready: false, subscriptions: [] },
  repository: { status: 'verified', tasks: [{ name: 'web', display_name: 'Web App', status: 'verified' }] },
};

@Component({
  selector: 'app-sg-patterns',
  standalone: true,
  imports: [
    PageLayoutComponent, RowListComponent, RowComponent, CardGridComponent,
    SettingsSectionComponent, FormFieldComponent, FieldRowComponent, ButtonComponent,
    BadgeComponent, InputDirective, FormsModule,
    NavCardComponent, SelectComponent, IconComponent, InputFetchListComponent, BuildProgressComponent,
    ServerConfigurationManagedLockButtonComponent,
    SettingsPageSideNavigationComponent,
    BreadcrumbComponent, LinkTabsComponent, ProjectReadinessCardComponent, WaitingNoticeComponent,
  ],
  // The demo setup can use sample data instead of real projects. Real teams remain unchanged after a click on the grant button.
  providers: [
    { provide: ProjectsService, useValue: { getProjectReadiness: () => of(sampleProjectReadiness) } },
    { provide: TeamsService, useValue: { grantProject: () => of('Team granted access'), updateProjectGrant: () => of('Workers enabled') } },
  ],
  templateUrl: './patterns.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './patterns.component.scss',
})
export class PatternsComponent {
  crumbs: Crumb[] = [
    { label: 'my-project', link: ['/styleguide'] },
    { label: 'Settings', link: ['/styleguide'] },
    { label: 'Git host connections' },
  ];
  settingsSideNavigationBreadcrumbTrail: Crumb[] = [
    { label: 'my-project', link: ['/styleguide'] },
    { label: 'Settings' },
  ];
  settingsSideNavigationLinks: VerticalNavigationLink[] = [
    { label: 'Foundations', icon: 'palette', link: ['/styleguide', 'foundations'] },
    { label: 'Elements', icon: 'widgets', link: ['/styleguide', 'elements'] },
    { label: 'Components', icon: 'view_module', link: ['/styleguide', 'components'], badge: '2 Pending' },
    { label: 'Patterns', icon: 'dashboard', link: ['/styleguide', 'patterns'], showSeparatorLineBeforeLink: true },
  ];
  role = signal('admin');
  roleOptions = [
    { label: 'Admin', value: 'admin' },
    { label: 'Viewer', value: 'viewer' },
  ];
  cards = [
    { name: 'gradient', status: 'passing', severity: 'success' as const },
    { name: 'nixpkgs-mirror', status: 'failed', severity: 'danger' as const },
    { name: 'infra', status: 'queued', severity: 'neutral' as const },
  ];
  inputFetches: InputFetch[] = [
    { name: 'nixpkgs', state: 'Fetching', downloaded_bytes: 18_400_000, expected_bytes: 46_000_000 },
    { name: 'home-manager', state: 'Fetching', downloaded_bytes: 2_100_000, expected_bytes: 0 },
    { name: 'flake-utils', state: 'Done', downloaded_bytes: 15_300, expected_bytes: 0 },
    { name: 'crane', state: 'Queued', downloaded_bytes: 0, expected_bytes: 0 },
    { name: 'private-overlay', state: 'Failed', downloaded_bytes: 0, expected_bytes: 0 },
  ];
  prefetch: BuildProgress = { phase: 'prefetch', bytes_done: 126_000_000, bytes_total: 356_000_000, paths_done: 12, paths_total: 40 };
  upload: BuildProgress = { phase: 'upload', bytes_done: 30 * 1024 * 1024, bytes_total: 80 * 1024 * 1024, paths_done: 1, paths_total: 3 };
  download: BuildProgress = { phase: 'download', bytes_done: 18_400_000, bytes_total: 46_000_000, paths_done: 0, paths_total: 1 };
  loadingBreadcrumbTrail: Crumb[] = [{ label: null, link: ['/styleguide'] }, { label: 'Settings' }];
  styleguideLinkTabSamples: LinkTab[] = [
    { label: 'Foundations', link: ['/styleguide', 'foundations'] },
    { label: 'Elements', link: ['/styleguide', 'elements'] },
    { label: 'Components', link: ['/styleguide', 'components'] },
    { label: 'Patterns', link: ['/styleguide', 'patterns'] },
  ];
  viewerProjectPermissions: Permission[] = ['viewProject', 'manageWorkers', 'manageSubscriptions', 'editTask'];
  projectAdminWaitingAccess = waitingAccessFrom(this.viewerProjectPermissions, true, false);
  outsideVisitorWaitingAccess = waitingAccessFrom([], false, false);
}
