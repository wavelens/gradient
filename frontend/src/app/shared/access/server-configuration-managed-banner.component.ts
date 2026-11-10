/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, booleanAttribute, computed, input } from '@angular/core';
import { MessageBannerComponent } from '@gradient/ui/ui';
import { AccessState } from '@core/models/access.model';
import type { CachePermission, Permission } from '@core/models';
import { getMembersWithPermissionPhrase } from './missing-permission';

@Component({
  selector: 'app-server-configuration-managed-banner',
  standalone: true,
  imports: [MessageBannerComponent],
  template: `
    @if (access()?.managed) {
      <gr-message-banner type="info">
        Settings for this {{ kind() }} come from the server configuration.
        @if (kind() === 'cache' && access()?.canEdit) { Deactivating the cache still works here.}
      </gr-message-banner>
    } @else if (showReadOnlyNote() && access() && !access()?.canEdit) {
      <gr-message-banner type="info">Only the {{ settingsEditors() }} can change these settings.</gr-message-banner>
    }
  `,
  styles: ':host { display: contents; }',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class ServerConfigurationManagedBannerComponent {
  access = input.required<AccessState | null | undefined>();
  kind = input.required<'project' | 'cache' | 'team' | 'task'>();
  showReadOnlyNote = input(false, { transform: booleanAttribute });
  permission = input<Permission | CachePermission>();

  protected settingsEditors = computed(() => {
    const kind = this.kind();
    return getMembersWithPermissionPhrase(kind === 'task' ? 'project' : kind, this.permission());
  });
}
