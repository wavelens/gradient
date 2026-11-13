/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, booleanAttribute, computed, input } from '@angular/core';
import { BadgeComponent, IconComponent, TooltipDirective } from '@gradient/ui/ui';
import { AccessState } from '@core/models/access.model';
import type { CachePermission, Permission } from '@core/models';
import { getMembersWithPermissionPhrase } from './missing-permission';

let chipInstanceIdCounter = 0;

@Component({
  selector: 'app-settings-access-state-badge',
  standalone: true,
  imports: [BadgeComponent, IconComponent, TooltipDirective],
  template: `
    @if (chipLabelLockIconAndTooltip(); as chip) {
      <button
        type="button"
        class="access-status-chip-button"
        [grTooltip]="chip.tooltip"
        tooltipPosition="bottom"
        [attr.aria-describedby]="chipTooltipDescriptionId"
      >
        <gr-badge>
          @if (chip.lockIcon) {
            <gr-icon name="lock" size="sm" />
          }
          {{ chip.label }}
        </gr-badge>
      </button>
      <span hidden [id]="chipTooltipDescriptionId">{{ chip.tooltip }}</span>
    }
  `,
  styleUrl: './settings-access-state-badge.component.scss',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class SettingsAccessStateBadgeComponent {
  access = input.required<AccessState | null | undefined>();
  kind = input.required<'project' | 'cache' | 'team' | 'task'>();
  showReadOnlyNote = input(false, { transform: booleanAttribute });
  permission = input<Permission | CachePermission>();

  protected chipTooltipDescriptionId = `settings-access-state-badge-tooltip-${chipInstanceIdCounter++}`;

  protected chipLabelLockIconAndTooltip = computed(() => {
    const access = this.access();
    const kind = this.kind();
    if (!access) return null;
    if (access.managed) {
      return {
        label: 'Managed',
        lockIcon: true,
        tooltip: `Settings for this ${kind} come from the server configuration.${kind === 'cache' && access.canEdit ? ' Deactivating the cache still works here.' : ''}`,
      };
    }
    if (!this.showReadOnlyNote() || access.canEdit) return null;
    const settingsEditors = getMembersWithPermissionPhrase(kind === 'task' ? 'project' : kind, this.permission());
    return { label: 'Read-only', lockIcon: false, tooltip: `Only the ${settingsEditors} can change these settings.` };
  });
}
