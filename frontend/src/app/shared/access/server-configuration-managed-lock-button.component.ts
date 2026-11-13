/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component } from '@angular/core';
import { IconComponent, TooltipDirective } from '@gradient/ui/ui';

@Component({
  selector: 'app-server-configuration-managed-lock-button',
  standalone: true,
  imports: [IconComponent, TooltipDirective],
  template: `
    <button
      type="button"
      class="row-lock-icon-button"
      aria-label="Set in the server's configuration file"
      grTooltip="Set in the server's configuration file"
    >
      <gr-icon name="lock" size="sm" />
    </button>
  `,
  styleUrl: './server-configuration-managed-lock-button.component.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class ServerConfigurationManagedLockButtonComponent {}
