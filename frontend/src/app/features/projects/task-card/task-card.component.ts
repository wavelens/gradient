/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, input } from '@angular/core';
import { RouterLink } from '@angular/router';
import { BadgeComponent, IconComponent } from '@gradient/ui/ui';
import type { TaskResponse } from '@core/models';
import { EvaluationHistoryComponent } from '@features/dashboard/evaluation-history/evaluation-history.component';
import { links } from '@shared/routing';
import { EvalStatusBadgeComponent, formatEntityLabel } from '@shared/ui';

@Component({
  selector: 'app-task-card',
  standalone: true,
  imports: [RouterLink, BadgeComponent, IconComponent, EvalStatusBadgeComponent, EvaluationHistoryComponent],
  templateUrl: './task-card.component.html',
  styleUrl: './task-card.component.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class TaskCardComponent {
  protected readonly links = links;
  protected readonly formatEntityLabel = formatEntityLabel;

  readonly task = input.required<TaskResponse>();
  readonly project = input.required<string>();
  readonly latestEvaluationStatusText = input<string | null>(null);
}
