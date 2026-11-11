/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, inject, input } from '@angular/core';
import { RouterLink } from '@angular/router';
import { MessageBannerComponent } from '@gradient/ui/ui';
import { PendingUserDecisionsService } from '@core/services/pending-invites.service';
import type { InviteKind } from '@core/models';
import { links } from '@shared/routing';
import { plural } from '@shared/text';

@Component({
  selector: 'app-pending-invitations-banner',
  standalone: true,
  imports: [MessageBannerComponent, RouterLink],
  template: `
    @if (count(); as count) {
      <gr-message-banner type="info" icon="mail">
        {{ plural(count, 'pending ' + kind() + ' invitation') }}
        <a [routerLink]="links.settings('invites')">Review invitations</a>
      </gr-message-banner>
    }
  `,
  styles: ':host { display: contents; }',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class PendingInvitationsBannerComponent {
  private pending = inject(PendingUserDecisionsService);
  readonly kind = input.required<InviteKind>();
  protected readonly count = computed(() => this.pending.invites().filter((invite) => invite.kind === this.kind()).length);
  protected readonly links = links;
  protected readonly plural = plural;
}
