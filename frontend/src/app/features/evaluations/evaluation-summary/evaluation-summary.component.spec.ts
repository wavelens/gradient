/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { of, throwError } from 'rxjs';
import type { Evaluation } from '@core/models';
import { EvaluationFailureSummary, EvaluationsService, FailedBuildSummary } from '@core/services/evaluations.service';
import { ProjectsService } from '@core/services/projects.service';
import { TasksService } from '@core/services/tasks.service';
import { EvaluationSummaryComponent } from './evaluation-summary.component';

const EVALUATION = {
  id: 'e2',
  task_name: 'testtask',
  task_display_name: 'MyTask',
  repository: 'https://github.com/NixOS/nixpkgs',
  commit: '8155901b00000000000000000000000000000000',
  status: 'Failed',
  created_at: '2026-10-10T06:59:00',
  started_at: '2026-10-10T06:59:01',
  finished_at: '2026-10-10T07:04:00',
  updated_at: '2026-10-10T07:04:00',
} as Evaluation;

const failure = (over: Partial<FailedBuildSummary>): FailedBuildSummary => ({
  build_id: 'b1',
  name: 'glibc-2.40',
  derivation_path: '/nix/store/x-glibc-2.40.drv',
  architecture: 'x86_64-linux',
  status: 'FailedPermanent',
  attributes: [],
  blocked: ['curl', 'git'],
  blocked_total: 2,
  newly_failing: true,
  ...over,
});

const SUMMARY: EvaluationFailureSummary = {
  compared_with: { id: 'e1', commit: 'aaaaaaaa11111111' },
  packages: { total: 20, built: 16, failed: 1, blocked: 2, unfinished: 0 },
  failures: [failure({}), failure({ build_id: 'b2', name: 'hello-2.12', attributes: ['hello'], blocked: [], blocked_total: 0, newly_failing: false })],
  failures_total: 2,
  failed_attributes: [{ attr: 'packages.broken', message: 'error: assertion failed\n  at flake.nix:9', newly_failing: true }],
  fixed: ['jq'],
  fixed_total: 1,
};

const getTaskInfo = vi.fn();

function setup(summary: EvaluationFailureSummary | null, evaluation = EVALUATION): ComponentFixture<EvaluationSummaryComponent> {
  TestBed.configureTestingModule({
    imports: [EvaluationSummaryComponent],
    providers: [
      provideRouter([]),
      { provide: ActivatedRoute, useValue: { snapshot: { paramMap: convertToParamMap({ project: 'testproject', evaluationId: 'e2' }) } } },
      { provide: ProjectsService, useValue: { getProject: () => of({ display_name: 'MyProject' }) } },
      { provide: TasksService, useValue: { getTaskInfo } },
      {
        provide: EvaluationsService,
        useValue: {
          getEvaluation: () => of(evaluation),
          getEvaluationSummary: () => (summary ? of(summary) : throwError(() => new Error('not found'))),
        },
      },
    ],
  });
  const fixture = TestBed.createComponent(EvaluationSummaryComponent);
  fixture.detectChanges();

  return fixture;
}

const text = (fixture: ComponentFixture<EvaluationSummaryComponent>) =>
  ((fixture.nativeElement as HTMLElement).textContent ?? '').replace(/\s+/g, ' ');

const crumbs = (fixture: ComponentFixture<EvaluationSummaryComponent>) =>
  Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('.breadcrumb-link, .breadcrumb-current')).map((crumb) => crumb.textContent?.trim());

const sections = (fixture: ComponentFixture<EvaluationSummaryComponent>) =>
  Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('h2')).map((h) => (h.textContent ?? '').replace(/\s+/g, ' ').trim());

describe('EvaluationSummaryComponent', () => {
  it('opens with the verdict and splits the failures by the compared evaluation', () => {
    const fixture = setup(SUMMARY);

    expect((fixture.nativeElement as HTMLElement).querySelector('h1')?.textContent).toContain('4 packages did not build');
    expect(sections(fixture)).toEqual(['Newly failing 1', 'Still failing 1', 'Failed to evaluate 1', 'Fixed 1']);
    expect(text(fixture)).toContain('2 newly failing and 1 fixed since aaaaaaaa');
  });

  it('links a failed build to that build on the log page', () => {
    const fixture = setup(SUMMARY);
    const link = (fixture.nativeElement as HTMLElement).querySelector('a.row-link');

    expect(link?.getAttribute('href')).toBe('/project/testproject/log/e2?build=b1');
    expect(text(fixture)).toContain('Blocks 2 packages: curl, git');
  });

  it('lists the failures as a single group without an evaluation to compare with', () => {
    const fixture = setup({ ...SUMMARY, compared_with: null, fixed: [], fixed_total: 0 });

    expect(sections(fixture)).toEqual(['Failed builds 2', 'Failed to evaluate 1']);
  });

  it('says so when nothing went wrong', () => {
    const fixture = setup({ ...SUMMARY, packages: { total: 20, built: 20, failed: 0, blocked: 0, unfinished: 0 }, failures: [], failures_total: 0, failed_attributes: [] });

    expect(text(fixture)).toContain('All 20 packages built');
    expect(text(fixture)).toContain('Nothing went wrong');
  });

  it('leads back through the task, named as the evaluation names it without a lookup', () => {
    expect(crumbs(setup(SUMMARY))).toEqual(['Projects', 'MyProject', 'MyTask', 'Summary']);
    expect(getTaskInfo).not.toHaveBeenCalled();
  });

  it('leads back to the project alone for an evaluation without a task', () => {
    const fixture = setup(SUMMARY, { ...EVALUATION, task_name: undefined, task_display_name: undefined });

    expect(crumbs(fixture)).toEqual(['Projects', 'MyProject', 'Summary']);
  });

  it('explains a summary that cannot be loaded and still leads back to the project', () => {
    const fixture = setup(null);

    expect(text(fixture)).toContain('Summary not available');
    expect(crumbs(fixture)).toEqual(['Projects', 'MyProject', 'Summary']);
  });
});
