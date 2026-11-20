/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, inject, viewChild } from '@angular/core';
import { Observable } from 'rxjs';
import { ButtonComponent, SettingsSectionComponent } from '@gradient/ui/ui';
import type { AccessState, GitHostConnectionOwner, GitHostType } from '@core/models';
import { ConfigService } from '@core/services/config.service';
import { SELF_HOSTED_GIT_HOSTS } from '@shared/git/git-host';
import { GitHostConnectionListComponent } from '@shared/git-host-connections/git-host-connection-list.component';
import { GithubAppComponent } from './github-app/github-app.component';

@Component({
  selector: 'app-admin-git-hosts-tab',
  standalone: true,
  imports: [GithubAppComponent, GitHostConnectionListComponent, SettingsSectionComponent, ButtonComponent],
  template: `
    <app-admin-github-app />
    <gr-settings-section
      title="Instance Connections"
      [card]="false"
    >
      @if (gitHostConnectionList()?.sortedConnections()?.length) {
        <button slot="actions" grButton label="New connection" icon="add" (click)="gitHostConnectionList()?.openCreate()"></button>
      }
      <app-git-host-connection-list [gitHostConnectionOwner]="instanceOwner" [access]="superuserAccess" [gitHostTypes]="gitHostTypes" />
    </gr-settings-section>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: '../admin-area.scss',
})
export class AdminGitHostsTabComponent {
  protected readonly instanceOwner: GitHostConnectionOwner = { type: 'instance' };
  protected readonly superuserAccess: AccessState = { managed: false, canEdit: true, canTrigger: true };
  protected readonly gitHostTypes: readonly GitHostType[] | null = inject(ConfigService).githubAppEnabled
    ? SELF_HOSTED_GIT_HOSTS.map((gitHost) => gitHost.value)
    : null;
  private readonly githubApp = viewChild.required(GithubAppComponent);
  protected readonly gitHostConnectionList = viewChild(GitHostConnectionListComponent);

  canDeactivate(): boolean | Observable<boolean> {
    return this.githubApp().canDeactivate();
  }
}
