/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { Subject, of, throwError } from 'rxjs';
import { ApiError } from '@core/services/api.service';
import { ConfirmDialogComponent } from './confirm-dialog.component';
import { ConfirmRequest, ConfirmService } from './confirm.service';

function setup() {
  TestBed.configureTestingModule({ imports: [ConfirmDialogComponent] });
  const fixture = TestBed.createComponent(ConfirmDialogComponent);
  fixture.detectChanges();
  const confirm = TestBed.inject(ConfirmService);
  const panel = () => document.querySelector<HTMLElement>('.gr-dialog');
  const button = (label: string) =>
    Array.from(panel()?.querySelectorAll<HTMLButtonElement>('button') ?? []).find((b) => b.textContent?.trim() === label);
  const ask = <T>(action: ConfirmRequest<T>['action'], isIrreversible = false) => {
    const results: T[] = [];
    let completed = false;
    confirm
      .confirm({
        title: 'Remove member',
        target: 'alice',
        consequence: 'alice loses access to this project.',
        confirmLabel: 'Remove',
        failure: 'Failed to remove the member.',
        isIrreversible,
        action,
      })
      .subscribe({ next: (value) => results.push(value), complete: () => (completed = true) });
    fixture.detectChanges();
    return { results, done: () => completed };
  };
  return { fixture, panel, button, ask };
}

describe('ConfirmDialogComponent', () => {
  it('names the target and the consequence before anything is sent', () => {
    const { panel, ask } = setup();
    const action = vi.fn(() => of('removed'));
    ask(action);
    expect(panel()?.textContent).toContain('Remove member');
    expect(panel()?.textContent).toContain('alice');
    expect(panel()?.textContent).toContain('alice loses access to this project.');
    expect(panel()?.textContent).not.toContain('permanent');
    expect(action).not.toHaveBeenCalled();
  });

  it('ends a permanent action with one fixed sentence', () => {
    const { panel, ask } = setup();
    ask(() => of('removed'), true);
    expect(panel()?.querySelectorAll('p')[1]?.textContent).toBe('alice loses access to this project. This action is permanent.');
  });

  it('sends nothing and completes empty when cancelled', () => {
    const { fixture, panel, button, ask } = setup();
    const action = vi.fn(() => of('removed'));
    const asked = ask(action);
    button('Cancel')!.click();
    fixture.detectChanges();
    expect(action).not.toHaveBeenCalled();
    expect(asked.results).toEqual([]);
    expect(asked.done()).toBe(true);
    expect(panel()).toBeNull();
  });

  it('stays open and locked while the action runs, then closes with its result', () => {
    const { fixture, panel, button, ask } = setup();
    const response = new Subject<string>();
    const asked = ask(() => response);
    button('Remove')!.click();
    fixture.detectChanges();
    panel()!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', keyCode: 27, bubbles: true }));
    button('Remove')!.click();
    fixture.detectChanges();
    expect(panel()).not.toBeNull();
    expect(panel()!.getAttribute('aria-busy')).toBe('true');

    response.next('removed');
    fixture.detectChanges();
    expect(asked.results).toEqual(['removed']);
    expect(asked.done()).toBe(true);
    expect(panel()).toBeNull();
  });

  it('hands the result over only after the dialog is gone, so callers can navigate', () => {
    const { fixture, panel, button } = setup();
    const panelOpenAtResult: boolean[] = [];
    TestBed.inject(ConfirmService)
      .confirm({ title: 'Delete', target: 'p', consequence: 'Gone.', confirmLabel: 'Delete', failure: 'No.', action: () => of(1) })
      .subscribe(() => panelOpenAtResult.push(panel() !== null));
    fixture.detectChanges();
    button('Delete')!.click();
    fixture.detectChanges();
    expect(panelOpenAtResult).toEqual([false]);
  });

  it('keeps a failure inside the dialog so the user can retry or cancel', () => {
    const { fixture, panel, button, ask } = setup();
    let attempts = 0;
    const asked = ask(() => (++attempts === 1 ? throwError(() => new ApiError('alice is the last admin', 409)) : of('removed')));
    button('Remove')!.click();
    fixture.detectChanges();
    expect(panel()?.textContent).toContain('alice is the last admin');
    expect(asked.done()).toBe(false);

    button('Remove')!.click();
    fixture.detectChanges();
    expect(attempts).toBe(2);
    expect(asked.results).toEqual(['removed']);
  });
});
