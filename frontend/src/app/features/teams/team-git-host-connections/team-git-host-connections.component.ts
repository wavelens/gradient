/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, inject } from '@angular/core';
import { ActivatedRoute } from '@angular/router';
import { LoadStateComponent, PageLayoutComponent } from '@gradient/ui/ui';
import { AccessState, GitHostConnectionOwner } from '@core/models';
import { AuthService } from '@core/services/auth.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { injectTeam } from '@core/resolvers/team-access.resolver';
import { ServerConfigurationManagedBannerComponent, canManageTeam, createNoAccessLoadStateError } from '@shared/access';
import { GitHostConnectionListComponent } from '@shared/git-host-connections/git-host-connection-list.component';
import { links } from '@shared/routing';

@Component({
  selector: 'app-team-git-host-connections',
  standalone: true,
  imports: [LoadStateComponent, PageLayoutComponent, ServerConfigurationManagedBannerComponent, GitHostConnectionListComponent],
  template: `
    <gr-page-layout width="settings" [breadcrumb]="breadcrumb()" title="Git host connections"
      subtitle="A connection that all projects in this team can use.">
      @if (isMember()) {
        <app-server-configuration-managed-banner [access]="access()" kind="team" showReadOnlyNote />
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
  private teamName = inject(ActivatedRoute).snapshot.paramMap.get('team') ?? '';

  protected readonly owner: GitHostConnectionOwner = { type: 'team', name: this.teamName };
  protected readonly noAccess = createNoAccessLoadStateError('team', 'members');
  isMember = computed(() => this.team()?.role != null || this.auth.user()?.superuser === true);
  access = computed<AccessState>(() => {
    const canEdit = canManageTeam(this.team(), this.auth.user());
    return { managed: this.team()?.managed ?? false, canEdit, canTrigger: canEdit };
  });
  breadcrumb = computed(() =>
    this.crumbs.team(this.teamName, { label: 'Settings', link: links.teamPage(this.teamName, 'settings') }, { label: 'Git host connections' }),
  );
}
