/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, inject } from '@angular/core';
import { LinkTabsComponent, type LinkTab } from '@gradient/ui/ui';
import { ConfigService } from '@core/services/config.service';
import { PendingInvitesService } from '@core/services/pending-invites.service';
import { links } from '@shared/routing';

@Component({
  selector: 'app-settings-nav',
  standalone: true,
  imports: [LinkTabsComponent],
  template: `<gr-link-tabs ariaLabel="Settings" [tabs]="tabs()" />`,
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class SettingsNavComponent {
  private config = inject(ConfigService);
  private invites = inject(PendingInvitesService);

  protected tabs = computed<LinkTab[]>(() => {
    const pending = this.invites.count();
    return [
      { label: 'Profile', link: links.settings('profile') },
      { label: 'API Keys', link: links.settings('keys') },
      ...(this.config.sshEnabled ? [{ label: 'SSH Keys', link: links.settings('ssh-keys') }] : []),
      { label: 'Sessions', link: links.settings('sessions') },
      { label: pending ? `Invitations (${pending})` : 'Invitations', link: links.settings('invites') },
      { label: 'Access', link: links.settings('access') },
    ];
  });
}
