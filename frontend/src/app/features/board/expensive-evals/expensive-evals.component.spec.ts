/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { of, throwError } from 'rxjs';
import { BoardExpensiveEvalsComponent } from './expensive-evals.component';
import { BoardService } from '@core/services/board.service';
import { ExpensiveEval } from '@core/models';

const EVALS: ExpensiveEval[] = [
  {
    evaluation: 'e1',
    project: 'p1',
    project_name: 'acme',
    project_display_name: 'Acme Corp',
    task_name: 'nixos',
    task_display_name: 'NixOS Systems',
    name: 'nixosConfigurations.*',
    value: 1200,
    unit: 'ms',
    worker: 'w-uuid-1',
    worker_name: 'builder-1',
  },
  {
    evaluation: 'e2',
    project: 'p2',
    project_name: 'infra',
    project_display_name: '',
    task_name: 'ci',
    task_display_name: '',
    name: 'packages.**',
    value: 800,
    unit: 'ms',
    worker: 'w-uuid-2',
    worker_name: null,
  },
];

function rows(): HTMLTableRowElement[] {
  TestBed.configureTestingModule({
    imports: [BoardExpensiveEvalsComponent],
    providers: [
      provideRouter([]),
      { provide: BoardService, useValue: { getExpensiveEvalsByResource: () => of(EVALS) } },
    ],
  });
  const fixture = TestBed.createComponent(BoardExpensiveEvalsComponent);
  fixture.detectChanges();
  return Array.from(fixture.nativeElement.querySelectorAll('tbody tr'));
}

describe('BoardExpensiveEvalsComponent', () => {
  it('shows the reason of a failed load instead of an empty table and loads again on Try Again', () => {
    const getExpensiveEvalsByResource = vi.fn()
      .mockReturnValueOnce(throwError(() => new Error('metrics store is down')))
      .mockReturnValue(of(EVALS));
    TestBed.configureTestingModule({
      imports: [BoardExpensiveEvalsComponent],
      providers: [provideRouter([]), { provide: BoardService, useValue: { getExpensiveEvalsByResource } }],
    });
    const fixture = TestBed.createComponent(BoardExpensiveEvalsComponent);
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.textContent).toContain('Failed to load evaluations');
    expect(root.textContent).toContain('metrics store is down');
    expect(root.querySelectorAll('tbody tr')).toHaveLength(0);

    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')?.click();
    fixture.detectChanges();
    expect(root.querySelectorAll('tbody tr')).toHaveLength(2);
  });

  it('links each evaluation to its project and task by display name', () => {
    const links = Array.from(rows()[0].cells[1].querySelectorAll('a'));
    expect(links.map((a) => a.textContent?.trim())).toEqual(['Acme Corp', 'NixOS Systems']);
    expect(links.map((a) => a.getAttribute('href'))).toEqual(['/project/acme', '/project/acme/task/nixos']);
  });

  it('opens the log of each evaluation, so rows for the same task stay apart', () => {
    expect(rows().map((r) => r.cells[2].querySelector('a')?.getAttribute('href'))).toEqual(['/project/acme/log/e1', '/project/infra/log/e2']);
  });

  it('falls back to the names when no display name is set', () => {
    const links = Array.from(rows()[1].cells[1].querySelectorAll('a'));
    expect(links.map((a) => a.textContent?.trim())).toEqual(['infra', 'ci']);
  });

  it('shows the worker display name, falling back to its id', () => {
    expect(rows().map((r) => r.cells[4].textContent?.trim())).toEqual(['builder-1', 'w-uuid-2']);
  });
});
