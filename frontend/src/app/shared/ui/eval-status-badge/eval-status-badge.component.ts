/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, computed, input, ChangeDetectionStrategy } from '@angular/core';
import { evaluationPhase, getEvaluationStatusLabel } from '@shared/evaluation';
import { StatusBadgeComponent } from '../status-badge/status-badge.component';
import type { EvaluationStatus, WaitingReason } from '@core/models';

@Component({
  selector: 'gr-eval-status-badge',
  standalone: true,
  imports: [StatusBadgeComponent],
  template: `<gr-status-badge [phase]="phase()" [label]="label()" [prioritized]="prioritized()" />`,
  styles: ':host { display: inline-flex; }',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class EvalStatusBadgeComponent {
  status = input.required<EvaluationStatus>();
  prioritized = input(false);
  waitingReason = input<WaitingReason | null | undefined>(null);

  phase = computed(() => evaluationPhase(this.status(), this.waitingReason()));

  label = computed(() => getEvaluationStatusLabel(this.status(), this.waitingReason()));
}
