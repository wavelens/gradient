/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';
import { BadgeComponent, type BadgeSeverity } from '@gradient/ui/ui';
import { TaskGitHostChainStepState } from '@core/models';

const STATE_BADGES: Record<TaskGitHostChainStepState, { label: string; severity: BadgeSeverity }> = {
  ok: { label: 'Working', severity: 'success' },
  attention: { label: 'Incomplete', severity: 'warning' },
  failing: { label: 'Failing', severity: 'danger' },
  off: { label: 'Off', severity: 'neutral' },
};

@Component({
  selector: 'app-task-repository-status-check-step-state-badge',
  standalone: true,
  imports: [BadgeComponent],
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `<gr-badge [severity]="badge().severity">{{ badge().label }}</gr-badge>`,
})
export class TaskRepositoryStatusCheckStepStateBadgeComponent {
  state = input.required<TaskGitHostChainStepState>();
  protected badge = computed(() => STATE_BADGES[this.state()]);
}
