/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, inject, viewChild } from '@angular/core';
import { ActivatedRoute } from '@angular/router';
import { ButtonComponent, LoadStateComponent, PageLayoutComponent } from '@gradient/ui/ui';
import { AccessState, GitHostConnectionOwner } from '@core/models';
import { AuthService } from '@core/services/auth.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { injectTeam } from '@core/resolvers/team-access.resolver';
import { SettingsAccessStateBadgeComponent, WritableDirective, canManageTeam, createNoAccessLoadStateError } from '@shared/access';
import { GitHostConnectionListComponent } from '@shared/git-host-connections/git-host-connection-list.component';
import { TeamSettingsNavigationComponent } from '../team-settings-navigation/team-settings-navigation.component';

@Component({
  selector: 'app-team-git-host-connections',
  standalone: true,
  imports: [TeamSettingsNavigationComponent, LoadStateComponent, PageLayoutComponent, SettingsAccessStateBadgeComponent, GitHostConnectionListComponent, WritableDirective, ButtonComponent],
  template: `
    <gr-page-layout width="settings" [breadcrumb]="breadcrumb()" title="Git host connections">
      <app-team-settings-navigation slot="settings-navigation" [teamUrlName]="teamName" />
      @if (isMember()) {
        <app-settings-access-state-badge slot="after-title" [access]="access()" kind="team" showReadOnlyNote />
      }
      @if (gitHostConnectionList()?.ownConnections()?.length) {
        <button slot="actions" *appWritable="access()" grButton label="New connection" icon="add"
          (click)="gitHostConnectionList()?.openCreate()"></button>
      }
      @if (isMember()) {
        <app-git-host-connection-list [gitHostConnectionOwner]="owner" [access]="access()" />
      } @else {
        <gr-load-state [error]="noAccess" />
      }
    </gr-page-layout>
  `,
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class TeamGitHostConnectionsComponent {
  private crumbs = inject(BreadcrumbsService);
  private auth = inject(AuthService);
  private team = injectTeam();
  protected teamName = inject(ActivatedRoute).snapshot.paramMap.get('team') ?? '';

  protected readonly owner: GitHostConnectionOwner = { type: 'team', name: this.teamName };
  protected readonly noAccess = createNoAccessLoadStateError('team', 'members');
  isMember = computed(() => this.team()?.role != null || this.auth.user()?.superuser === true);
  access = computed<AccessState>(() => {
    const canEdit = canManageTeam(this.team(), this.auth.user());
    return { managed: this.team()?.managed ?? false, canEdit, canTrigger: canEdit };
  });
  breadcrumb = computed(() =>
    this.crumbs.teamSettings(this.teamName),
  );
  protected readonly gitHostConnectionList = viewChild(GitHostConnectionListComponent);
}
