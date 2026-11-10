/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, input, ChangeDetectionStrategy } from '@angular/core';
import { BadgeComponent } from '@gradient/ui/ui';

@Component({
  selector: 'gr-live-status',
  standalone: true,
  imports: [BadgeComponent],
  template: `
    @let description = connected() ? 'Live updates connected' : 'Live updates disconnected. Reconnecting. Values can be stale.';
    <span role="status" [attr.aria-label]="description" [title]="description">
      @if (connected()) {
        <gr-badge severity="success">Live</gr-badge>
      } @else {
        <gr-badge severity="warning">Reconnecting</gr-badge>
      }
    </span>
  `,
  styles: [':host { display: flex; justify-content: flex-end; min-width: 7rem; margin-left: auto; margin-bottom: 0.5rem; }'],
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class LiveStatusComponent {
  connected = input.required<boolean>();
}
