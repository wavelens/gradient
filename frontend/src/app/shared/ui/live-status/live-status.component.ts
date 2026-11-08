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
    <span role="status">
      @if (connected()) {
        <gr-badge severity="success">Live</gr-badge>
      } @else {
        <gr-badge severity="warning" title="Live connection dropped. Displayed values can be stale until reconnection.">Reconnecting</gr-badge>
      }
    </span>
  `,
  styles: [':host { display: flex; justify-content: flex-end; margin-left: auto; margin-bottom: 0.5rem; }'],
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class LiveStatusComponent {
  connected = input.required<boolean>();
}
