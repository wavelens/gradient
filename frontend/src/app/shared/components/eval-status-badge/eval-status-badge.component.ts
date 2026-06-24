/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, computed, input } from '@angular/core';
import { CommonModule } from '@angular/common';
import { EvaluationStatus } from '@core/models/project.model';
import { isRunningEvaluationStatus } from '@shared/evaluation';

@Component({
  selector: 'app-eval-status-badge',
  standalone: true,
  imports: [CommonModule],
  template: `
    <span class="eval-status-badge" [class]="statusClass()">
      <span class="material-symbols-outlined" [class.spinning]="spinning()" [class.pulsing]="pulsing()">
        {{ icon() }}
      </span>
      {{ label() }}
    </span>
  `,
  styleUrl: './eval-status-badge.component.scss',
})
export class EvalStatusBadgeComponent {
  status = input.required<EvaluationStatus>();

  statusClass = computed(() => {
    switch (this.status()) {
      case 'Completed': return 'status-success';
      case 'Failed': return 'status-danger';
      case 'Aborted': return 'status-secondary';
      case 'Waiting': return 'status-warning';
      default: return 'status-running';
    }
  });

  icon = computed(() => {
    switch (this.status()) {
      case 'Completed': return 'check_circle';
      case 'Failed': return 'error';
      case 'Aborted': return 'cancel';
      case 'Queued': return 'hourglass_empty';
      case 'Waiting': return 'pause_circle';
      default: return 'sync';
    }
  });

  label = computed(() => {
    const s = this.status();
    if (s === 'EvaluatingFlake' || s === 'EvaluatingDerivation') return 'Evaluating';
    return s;
  });

  pulsing = computed(() => this.status() === 'Queued' || this.status() === 'Waiting');
  spinning = computed(() => isRunningEvaluationStatus(this.status()) && !this.pulsing());
}
