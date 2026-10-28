/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, computed, input, ChangeDetectionStrategy } from '@angular/core';
import { evaluationPhase } from '@shared/evaluation';
import { StatusBadgeComponent } from '../status-badge/status-badge.component';
import { EvaluationStatus } from '@core/models';

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

  phase = computed(() => evaluationPhase(this.status()));

  label = computed(() => {
    const s = this.status();
    if (s === 'EvaluatingFlake' || s === 'EvaluatingDerivation') return 'Evaluating';
    return s;
  });
}
