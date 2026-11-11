/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, viewChild } from '@angular/core';
import { Observable } from 'rxjs';
import { SettingsSectionComponent } from '@gradient/ui/ui';
import type { AccessState, GitHostConnectionOwner } from '@core/models';
import { GitHostConnectionListComponent } from '@shared/git-host-connections/git-host-connection-list.component';
import { GithubAppComponent } from './github-app/github-app.component';

@Component({
  selector: 'app-admin-git-hosts-tab',
  standalone: true,
  imports: [GithubAppComponent, GitHostConnectionListComponent, SettingsSectionComponent],
  template: `
    <app-admin-github-app />
    <gr-settings-section
      title="Instance Connections"
      description="All projects on this instance can use these connections. Installations of GitHub Apps add their connections here."
      [card]="false"
    >
      <app-git-host-connection-list [gitHostConnectionOwner]="instanceOwner" [access]="superuserAccess" />
    </gr-settings-section>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: '../admin-area.scss',
})
export class AdminGitHostsTabComponent {
  protected readonly instanceOwner: GitHostConnectionOwner = { type: 'instance' };
  protected readonly superuserAccess: AccessState = { managed: false, canEdit: true, canTrigger: true };
  private readonly githubApp = viewChild.required(GithubAppComponent);

  canDeactivate(): boolean | Observable<boolean> {
    return this.githubApp().canDeactivate();
  }
}
