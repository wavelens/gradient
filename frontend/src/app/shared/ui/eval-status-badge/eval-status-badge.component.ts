/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, computed, input, ChangeDetectionStrategy } from '@angular/core';
import { EvaluationStatus } from '@core/models/task.model';
import { evaluationPhase } from '@shared/evaluation';
import { StatusIconComponent } from '../status-icon/status-icon.component';

@Component({
  selector: 'gr-eval-status-badge',
  standalone: true,
  imports: [StatusIconComponent],
  template: `
    <span class="eval-status-badge" [attr.data-phase]="phase()">
      <gr-status-icon [phase]="phase()" size="sm" />
      {{ label() }}
    </span>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './eval-status-badge.component.scss',
})
export class EvalStatusBadgeComponent {
  status = input.required<EvaluationStatus>();

  phase = computed(() => evaluationPhase(this.status()));

  label = computed(() => {
    const s = this.status();
    if (s === 'EvaluatingFlake' || s === 'EvaluatingDerivation') return 'Evaluating';
    return s;
  });
}
