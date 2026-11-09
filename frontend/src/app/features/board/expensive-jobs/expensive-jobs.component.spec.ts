/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { of } from 'rxjs';
import { vi } from 'vitest';
import { BoardExpensiveJobsComponent } from './expensive-jobs.component';
import { BoardService } from '@core/services/board.service';
import { AuthService } from '@core/services/auth.service';
import { ExpensiveBuild, ExpensiveResource, TopProjectBuildTime } from '@core/models';

const BUILDS: ExpensiveBuild[] = [
  { build_id: 'b1', project: 'p1', project_slug: 'nixpkgs', project_display_name: 'Nixpkgs', task_name: 'ci', task_display_name: 'CI', evaluation: 'e1', name: 'named', build_time_ms: 5000, worker: 'uuid-1', worker_name: 'builder-1' },
  { build_id: 'b2', project: 'p1', project_slug: 'nixpkgs', project_display_name: 'Nixpkgs', task_name: 'ci', task_display_name: 'CI', evaluation: 'e1', name: 'unnamed', build_time_ms: 4000, worker: 'uuid-2', worker_name: null },
  { build_id: 'b3', project: 'p1', project_slug: 'nixpkgs', project_display_name: 'Nixpkgs', task_name: 'ci', task_display_name: 'CI', evaluation: 'e1', name: 'unknown', build_time_ms: 3000, worker: null, worker_name: null },
];

const RESOURCES: ExpensiveResource[] = [
  { derivation: 'd1', project: 'p1', project_slug: 'nixpkgs', evaluation: 'e1', name: 'named', value: 10, unit: 'MB', worker: 'uuid-1', worker_name: 'builder-1' },
  { derivation: 'd2', project: 'p1', project_slug: 'nixpkgs', evaluation: 'e1', name: 'unnamed', value: 5, unit: 'MB', worker: 'uuid-2', worker_name: null },
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
      provideRouter([]),
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

  it('links each build to its place in the evaluation log', () => {
    const fixture = setup();
    const link = fixture.nativeElement.querySelector('a.build-link') as HTMLAnchorElement;
    expect(link.getAttribute('href')).toBe('/project/nixpkgs/log/e1?build=b1');
  });

  it('links each resource row to the evaluation log that built it', () => {
    const fixture = setup();
    fixture.componentInstance.setTab('ram');
    fixture.detectChanges();
    const link = fixture.nativeElement.querySelector('a.build-link') as HTMLAnchorElement;
    expect(link.getAttribute('href')).toBe('/project/nixpkgs/log/e1');
  });

  it('labels the top projects chart with display names, falling back to the project name', () => {
    const fixture = setup(true);
    expect(fixture.componentInstance.topProjectCategories()).toEqual(['Nixpkgs', 'infra']);
    const heading = fixture.nativeElement.querySelector('h2').textContent;
    expect(heading).toContain('Top projects by build time');
    expect(heading).not.toContain('superuser');
  });

  it('leaves the build time chart out of the resource rankings', () => {
    const fixture = setup(true);
    fixture.componentInstance.setTab('ram');
    fixture.detectChanges();
    expect(fixture.nativeElement.querySelector('h2')).toBeNull();
  });

  it('neither asks for nor shows the top projects to a non-superuser', () => {
    const fixture = setup();
    expect(getTopProjects).not.toHaveBeenCalled();
    expect(fixture.nativeElement.querySelector('h2')).toBeNull();
  });
});
