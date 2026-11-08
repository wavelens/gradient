/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, input } from '@angular/core';
import { MessageBannerComponent } from '@gradient/ui/ui';
import { AccessState } from '@core/models/access.model';

@Component({
  selector: 'app-server-configuration-managed-banner',
  standalone: true,
  imports: [MessageBannerComponent],
  template: `
    @if (access()?.managed) {
      <gr-message-banner type="info">This {{ kind() }} is managed by the server configuration and cannot be changed here.</gr-message-banner>
    }
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class ServerConfigurationManagedBannerComponent {
  access = input.required<AccessState | null | undefined>();
  kind = input.required<'project' | 'cache' | 'team' | 'task'>();
}
