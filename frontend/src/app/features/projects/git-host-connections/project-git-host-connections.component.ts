/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, inject, viewChild } from '@angular/core';
import { ActivatedRoute } from '@angular/router';
import { ButtonComponent, LoadStateComponent, PageLayoutComponent } from '@gradient/ui/ui';
import { accessTo, GitHostConnectionOwner } from '@core/models';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { injectProject } from '@core/resolvers/project-access.resolver';
import { SettingsAccessStateBadgeComponent, WritableDirective, createMissingPermissionLoadStateError, isViewerProjectMember } from '@shared/access';
import { GitHostConnectionListComponent } from '@shared/git-host-connections/git-host-connection-list.component';
import { ProjectSettingsNavigationComponent } from '../project-settings-navigation/project-settings-navigation.component';

@Component({
  selector: 'app-project-git-host-connections',
  standalone: true,
  imports: [ProjectSettingsNavigationComponent, LoadStateComponent, PageLayoutComponent, SettingsAccessStateBadgeComponent, GitHostConnectionListComponent, WritableDirective, ButtonComponent],
  template: `
    <gr-page-layout width="settings" [breadcrumb]="breadcrumb()" title="Git host connections">
      <app-project-settings-navigation slot="settings-navigation" [projectUrlName]="projectName" />
      @if (isMember()) {
        <app-settings-access-state-badge slot="after-title" [access]="access()" kind="project" showReadOnlyNote permission="manageIntegrations" />
      }
      @if (gitHostConnectionList()?.ownConnections()?.length) {
        <button slot="actions" *appWritable="access()" grButton label="New connection" icon="add"
          (click)="gitHostConnectionList()?.openCreate()"></button>
      }
      @if (isMember()) {
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
  protected projectName = inject(ActivatedRoute).snapshot.paramMap.get('project') ?? '';

  protected readonly owner: GitHostConnectionOwner = { type: 'project', name: this.projectName };
  protected readonly missingPermission = createMissingPermissionLoadStateError('project', false);
  access = computed(() => accessTo(this.resolvedProject(), 'manageIntegrations'));
  isMember = computed(() => isViewerProjectMember(this.resolvedProject()));
  breadcrumb = computed(() => this.crumbs.projectSettings(this.projectName));
  protected readonly gitHostConnectionList = viewChild(GitHostConnectionListComponent);
}
