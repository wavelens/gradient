/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, ElementRef, computed, inject, input, output, viewChildren } from '@angular/core';
import { TooltipDirective } from '@gradient/ui/ui';
import { PHASE_COLOR } from '@shared/evaluation';
import { CurrentTimeSignal } from '@shared/text';
import { attemptRowTooltipText, formatAttemptDuration, type AttemptLogRow } from './attempt-log-row';

@Component({
  selector: 'gr-log-attempt-list',
  standalone: true,
  imports: [TooltipDirective],
  templateUrl: './log-attempt-list.component.html',
  styleUrl: './log-attempt-list.component.scss',
  changeDetection: ChangeDetectionStrategy.Eager,
  host: { role: 'group', 'aria-label': 'Attempts' },
})
export class LogAttemptListComponent {
  attemptRows = input.required<readonly AttemptLogRow[]>();
  attemptNumberForLogOnScreenOrNull = input<number | null>(null);
  selectAttempt = output<number>();
  arrowKeyPressedAtAttemptListBoundary = output<1 | -1>();

  private clock = inject(CurrentTimeSignal);
  private attemptRowButtons = viewChildren<ElementRef<HTMLButtonElement>>('attemptRowButtons');

  attemptRowsNewestFirstWithDurationTextTooltipTextAndDotColour = computed(() => {
    const currentTimeMilliseconds = this.clock.currentTimeMilliseconds();
    return [...this.attemptRows()].sort((a, b) => b.number - a.number).map((row) => ({
      ...row,
      durationText: formatAttemptDuration(row, currentTimeMilliseconds),
      finishTimeAndWorkerNameTooltip: attemptRowTooltipText(row),
      statusDotColor: PHASE_COLOR[row.statusPhase],
    }));
  });

  focusFirstAttemptRow(): void {
    this.attemptRowButtons()[0]?.nativeElement.focus();
  }

  focusAdjacentAttemptRowOrEmitArrowKeyPastListEnd(event: Event, index: number, step: 1 | -1): void {
    event.preventDefault();
    const buttonElementOfAttemptRowAboveOrBelowFocusedRow = this.attemptRowButtons()[index + step];
    if (buttonElementOfAttemptRowAboveOrBelowFocusedRow) buttonElementOfAttemptRowAboveOrBelowFocusedRow.nativeElement.focus();
    else this.arrowKeyPressedAtAttemptListBoundary.emit(step);
  }
}
