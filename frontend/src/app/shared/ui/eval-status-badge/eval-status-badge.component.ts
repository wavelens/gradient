/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, computed, input, ChangeDetectionStrategy } from '@angular/core';
import { evaluationPhase } from '@shared/evaluation';
import { StatusBadgeComponent } from '../status-badge/status-badge.component';
import type { EvaluationStatus, WaitingReason } from '@core/models';

@Component({
  selector: 'gr-eval-status-badge',
  standalone: true,
  imports: [StatusBadgeComponent],
  template: `<gr-status-badge [phase]="phase()" [label]="label()" [prioritized]="prioritized()" />`,
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class EvalStatusBadgeComponent {
  status = input.required<EvaluationStatus>();
  prioritized = input(false);
  waitingReason = input<WaitingReason | null | undefined>(null);

  phase = computed(() => evaluationPhase(this.status(), this.waitingReason()));

  label = computed(() => {
    const s = this.status();
    if (this.phase() === 'waiting') return 'Waiting';
    if (s === 'Fetching') return 'Fetching flake';
    if (s === 'EvaluatingFlake' || s === 'EvaluatingDerivation') return 'Evaluating';
    return s;
  });
}
