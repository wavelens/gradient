/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, inject, input } from '@angular/core';
import { SettingsPageSideNavigationComponent, type VerticalNavigationLink } from '@gradient/ui/ui';
import { injectTeam } from '@core/resolvers/team-access.resolver';
import { AuthService } from '@core/services/auth.service';
import { canManageTeam } from '@shared/access';
import { links } from '@shared/routing';

@Component({
  selector: 'app-team-settings-navigation',
  standalone: true,
  imports: [SettingsPageSideNavigationComponent],
  template: `<gr-settings-page-side-navigation ariaLabel="Team settings" [links]="navigationLinkItems()" />`,
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class TeamSettingsNavigationComponent {
  teamUrlName = input.required<string>();

  private authService = inject(AuthService);
  private team = injectTeam();

  protected navigationLinkItems = computed<VerticalNavigationLink[]>(() => {
    const team = this.teamUrlName();
    return [
      { label: 'General', icon: 'tune', link: links.teamPage(team, 'settings') },
      { label: 'Members', icon: 'group', link: links.teamPage(team, 'members') },
      { label: 'Git host connections', icon: 'hub', link: links.teamPage(team, 'git-host-connections') },
      ...(canManageTeam(this.team(), this.authService.user())
        ? [{ label: 'Event notifications', icon: 'webhook', link: links.teamPage(team, 'webhooks') }]
        : []),
    ];
  });
}
