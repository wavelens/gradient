/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import type { AttemptLogRow } from './attempt-log-row';
import { LogAttemptListComponent } from './log-attempt-list.component';

const ATTEMPT_ROWS: AttemptLogRow[] = [
  { number: 1, status: 'worker lost', statusPhase: 'failure', attemptStartedAt: '2026-01-01T00:00:01', attemptEndedAt: '2026-01-01T00:02:33' },
  { number: 2, status: 'timed out', statusPhase: 'failure', attemptStartedAt: '2026-01-01T00:03:00', attemptEndedAt: '2026-01-01T00:08:00' },
  { number: 3, status: 'built', statusPhase: 'success', attemptStartedAt: '2026-01-01T00:09:00', attemptEndedAt: '2026-01-01T00:09:01' },
];

function setup(attemptNumberForLogOnScreenOrNull: number | null = 3) {
  TestBed.configureTestingModule({ imports: [LogAttemptListComponent] });
  const fixture = TestBed.createComponent(LogAttemptListComponent);
  fixture.componentRef.setInput('attemptRows', ATTEMPT_ROWS);
  fixture.componentRef.setInput('attemptNumberForLogOnScreenOrNull', attemptNumberForLogOnScreenOrNull);
  fixture.detectChanges();
  const attemptRowButtons = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll<HTMLButtonElement>('button.sidebar-attempt-row-button'));
  return { fixture, list: fixture.componentInstance, attemptRowButtons };
}

const pressKey = (button: HTMLButtonElement, key: string) =>
  button.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true }));

describe('LogAttemptListComponent', () => {
  it('list attempts with result and duration in newest-first order', () => {
    const { attemptRowButtons } = setup();
    expect(attemptRowButtons.map((button) => Array.from(button.children, (part) => part.textContent?.trim()).filter(Boolean))).toEqual([
      ['Attempt 3 · built', '1.0 s'],
      ['Attempt 2 · timed out', '5m 00s'],
      ['Attempt 1 · worker lost', '2m 32s'],
    ]);
  });

  it('mark the attempt on screen from the log as current', () => {
    const { attemptRowButtons } = setup(2);
    expect(attemptRowButtons.map((button) => button.getAttribute('aria-current'))).toEqual([null, 'true', null]);
  });

  it('emit the attempt number of the clicked row', () => {
    const { list, attemptRowButtons } = setup();
    const picked: number[] = [];
    list.selectAttempt.subscribe((number) => picked.push(number));
    attemptRowButtons[2].click();
    expect(picked).toEqual([1]);
  });

  it('move focus between rows with arrow keys and report arrow keys past the first or last row', () => {
    const { fixture, list, attemptRowButtons } = setup();
    document.body.appendChild(fixture.nativeElement);
    const stepsPastListEnd: number[] = [];
    list.arrowKeyPressedAtAttemptListBoundary.subscribe((step) => stepsPastListEnd.push(step));

    list.focusFirstAttemptRow();
    expect(document.activeElement).toBe(attemptRowButtons[0]);
    pressKey(attemptRowButtons[0], 'ArrowDown');
    expect(document.activeElement).toBe(attemptRowButtons[1]);
    pressKey(attemptRowButtons[1], 'ArrowUp');
    expect(document.activeElement).toBe(attemptRowButtons[0]);
    expect(stepsPastListEnd).toEqual([]);

    pressKey(attemptRowButtons[0], 'ArrowUp');
    pressKey(attemptRowButtons[2], 'ArrowDown');
    expect(stepsPastListEnd).toEqual([-1, 1]);
    fixture.nativeElement.remove();
  });
});
