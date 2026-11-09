/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, inject } from '@angular/core';
import { RouterLink } from '@angular/router';
import { MessageBannerComponent } from '@gradient/ui/ui';
import { PendingInvitesService } from '@core/services/pending-invites.service';
import { links } from '@shared/routing';
import { plural } from '@shared/text';

@Component({
  selector: 'app-pending-invitations-banner',
  standalone: true,
  imports: [MessageBannerComponent, RouterLink],
  template: `
    @if (invites.count(); as count) {
      <gr-message-banner type="info" icon="mail">
        {{ plural(count, 'pending invitation') }}
        <a [routerLink]="links.settings('invites')">Review invitations</a>
      </gr-message-banner>
    }
  `,
  styles: ':host { display: contents; }',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class PendingInvitationsBannerComponent {
  protected invites = inject(PendingInvitesService);
  protected readonly links = links;
  protected readonly plural = plural;
}
