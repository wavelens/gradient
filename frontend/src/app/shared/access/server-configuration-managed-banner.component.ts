/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, booleanAttribute, computed, input } from '@angular/core';
import { MessageBannerComponent } from '@gradient/ui/ui';
import { AccessState } from '@core/models/access.model';

@Component({
  selector: 'app-server-configuration-managed-banner',
  standalone: true,
  imports: [MessageBannerComponent],
  template: `
    @if (access()?.managed && access()?.canEdit) {
      <gr-message-banner type="info">
        Settings for this {{ kind() }} come from the server configuration.
        @if (kind() === 'cache' || kind() === 'team') { Deactivating and adding extra members still work here.}
      </gr-message-banner>
    } @else if (showReadOnlyNote() && access() && !access()?.canEdit) {
      <gr-message-banner type="info">You can view these settings, but you can't change them. Contact an admin for this {{ settingsAdminEntityKind() }}.</gr-message-banner>
    }
  `,
  styles: ':host { display: contents; }',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class ServerConfigurationManagedBannerComponent {
  access = input.required<AccessState | null | undefined>();
  kind = input.required<'project' | 'cache' | 'team' | 'task'>();
  showReadOnlyNote = input(false, { transform: booleanAttribute });

  protected settingsAdminEntityKind = computed(() => (this.kind() === 'task' ? 'project' : this.kind()));
}
