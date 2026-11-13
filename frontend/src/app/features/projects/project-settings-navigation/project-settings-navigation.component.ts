/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, inject, input } from '@angular/core';
import { SettingsPageSideNavigationComponent, type VerticalNavigationLink } from '@gradient/ui/ui';
import { accessTo } from '@core/models';
import { injectProject } from '@core/resolvers/project-access.resolver';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { canOpenProjectMembersPage, isViewerProjectMember } from '@shared/access';
import { links } from '@shared/routing';

@Component({
  selector: 'app-project-settings-navigation',
  standalone: true,
  imports: [SettingsPageSideNavigationComponent],
  template: `<gr-settings-page-side-navigation ariaLabel="Project Settings" [links]="navigationLinkItems()" />`,
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class ProjectSettingsNavigationComponent {
  projectUrlName = input.required<string>();

  private authService = inject(AuthService);
  private config = inject(ConfigService);
  private project = injectProject();

  protected navigationLinkItems = computed<VerticalNavigationLink[]>(() => {
    const project = this.project();
    const name = this.projectUrlName();
    const general = { label: 'General', icon: 'tune', link: links.projectPage(name, 'settings') };
    const members = canOpenProjectMembersPage(project, this.authService.user())
      ? [{ label: 'Members', icon: 'group', link: links.projectPage(name, 'members') }]
      : [];
    if (!isViewerProjectMember(project)) return [general, ...members];
    return [
      general,
      ...members,
      { label: 'Workers', icon: 'construction', link: links.projectPage(name, 'workers') },
      { label: 'Cache subscriptions', icon: 'database', link: links.projectPage(name, 'caches') },
      { label: 'Git host connections', icon: 'hub', link: links.projectPage(name, 'git-host-connections') },
      ...(this.config.execEnabled && accessTo(project, 'manageProjectSettings').canEdit
        ? [{ label: 'Secrets', icon: 'key', link: links.projectPage(name, 'secrets') }]
        : []),
      ...(accessTo(project, 'manageWebhooks').canEdit
        ? [{ label: 'Event notifications', icon: 'webhook', link: links.projectPage(name, 'webhooks') }]
        : []),
    ];
  });
}
