/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { of } from 'rxjs';
import { vi } from 'vitest';
import { BoardExpensiveJobsComponent } from './expensive-jobs.component';
import {
  BoardService,
  ExpensiveBuild,
  ExpensiveResource,
  TopProjectBuildTime,
} from '@core/services/board.service';
import { AuthService } from '@core/services/auth.service';

const BUILDS: ExpensiveBuild[] = [
  { build_id: 'b1', project: 'p1', name: 'named', build_time_ms: 5000, worker: 'uuid-1', worker_name: 'builder-1' },
  { build_id: 'b2', project: 'p1', name: 'unnamed', build_time_ms: 4000, worker: 'uuid-2', worker_name: null },
  { build_id: 'b3', project: 'p1', name: 'unknown', build_time_ms: 3000, worker: null, worker_name: null },
];

const RESOURCES: ExpensiveResource[] = [
  { derivation: 'd1', project: 'p1', name: 'named', value: 10, unit: 'MB', worker: 'uuid-1', worker_name: 'builder-1' },
  { derivation: 'd2', project: 'p1', name: 'unnamed', value: 5, unit: 'MB', worker: 'uuid-2', worker_name: null },
];

const TOP: TopProjectBuildTime[] = [
  { project: '0123456789abcdef', project_name: 'nixpkgs', project_display_name: 'Nixpkgs', total_build_ms: 60000, build_count: 3 },
  { project: 'fedcba9876543210', project_name: 'infra', project_display_name: '', total_build_ms: 30000, build_count: 1 },
];

const getTopProjects = vi.fn(() => of(TOP));

function setup(superuser = false): ComponentFixture<BoardExpensiveJobsComponent> {
  getTopProjects.mockClear();
  TestBed.configureTestingModule({
    imports: [BoardExpensiveJobsComponent],
    providers: [
      {
        provide: BoardService,
        useValue: {
          getExpensive: () => of(BUILDS),
          getExpensiveByResource: () => of(RESOURCES),
          getTopProjects,
        },
      },
      { provide: AuthService, useValue: { user: () => ({ superuser }) } },
    ],
  });
  const fixture = TestBed.createComponent(BoardExpensiveJobsComponent);
  fixture.detectChanges();
  return fixture;
}

function workerCells(fixture: ComponentFixture<BoardExpensiveJobsComponent>): string[] {
  const rows = Array.from(fixture.nativeElement.querySelectorAll('tbody tr')) as HTMLElement[];
  return rows.map((r) => (r.querySelectorAll('td')[3] as HTMLElement).textContent!.trim());
}

describe('BoardExpensiveJobsComponent', () => {
  it('shows the worker name, falling back to its id', () => {
    const fixture = setup();
    expect(workerCells(fixture)).toEqual(['builder-1', 'uuid-2', '-']);
  });

  it('shows the worker name on the resource tabs', () => {
    const fixture = setup();
    fixture.componentInstance.setTab('ram');
    fixture.detectChanges();
    expect(workerCells(fixture)).toEqual(['builder-1', 'uuid-2']);
  });

  it('labels the top projects chart with display names, falling back to the project name', () => {
    const fixture = setup(true);
    expect(fixture.componentInstance.topProjectCategories()).toEqual(['Nixpkgs', 'infra']);
    const heading = fixture.nativeElement.querySelector('h2').textContent;
    expect(heading).toContain('Top projects by build time');
    expect(heading).not.toContain('superuser');
  });

  it('neither asks for nor shows the top projects to a non-superuser', () => {
    const fixture = setup();
    expect(getTopProjects).not.toHaveBeenCalled();
    expect(fixture.nativeElement.querySelector('h2')).toBeNull();
  });
});
