/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, inject } from '@angular/core';
import { ActivatedRoute } from '@angular/router';
import { LoadStateComponent, PageLayoutComponent } from '@gradient/ui/ui';
import { accessTo, GitHostConnectionOwner } from '@core/models';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { injectProject } from '@core/resolvers/project-access.resolver';
import { ServerConfigurationManagedBannerComponent, createMissingPermissionLoadStateError, isViewerProjectMember } from '@shared/access';
import { GitHostConnectionListComponent } from '@shared/git-host-connections/git-host-connection-list.component';

@Component({
  selector: 'app-project-git-host-connections',
  standalone: true,
  imports: [LoadStateComponent, PageLayoutComponent, ServerConfigurationManagedBannerComponent, GitHostConnectionListComponent],
  template: `
    <gr-page-layout width="settings" [breadcrumb]="breadcrumb()" title="Git host connections"
      subtitle="A connection to a Git host. This connection can receive events and report commit status for this project.">
      @if (isMember()) {
        <app-server-configuration-managed-banner [access]="access()" kind="project" showReadOnlyNote permission="manageIntegrations" />
        <app-git-host-connection-list [gitHostConnectionOwner]="owner" [access]="access()" />
      } @else {
        <gr-load-state [error]="missingPermission" />
      }
    </gr-page-layout>
  `,
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class ProjectGitHostConnectionsComponent {
  private crumbs = inject(BreadcrumbsService);
  private resolvedProject = injectProject();
  private projectName = inject(ActivatedRoute).snapshot.paramMap.get('project') ?? '';

  protected readonly owner: GitHostConnectionOwner = { type: 'project', name: this.projectName };
  protected readonly missingPermission = createMissingPermissionLoadStateError('project', false);
  access = computed(() => accessTo(this.resolvedProject(), 'manageIntegrations'));
  isMember = computed(() => isViewerProjectMember(this.resolvedProject()));
  breadcrumb = computed(() => this.crumbs.projectSettings(this.projectName, { label: 'Git host connections' }));
}
