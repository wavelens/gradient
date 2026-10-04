/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { HttpTestingController } from '@angular/common/http/testing';
import {
  BoardService,
  ExpensiveBuild,
  ExpensiveEval,
  ExpensiveResource,
  FlakeGraphNode,
  RuleDescription,
  TopProjectBuildTime,
} from './board.service';
import { environment } from '@environments/environment';

const apiUrl = environment.apiUrl;

const sampleEval: ExpensiveEval = {
  evaluation: 'eval-1',
  project: 'project-1',
  project_name: 'acme',
  project_display_name: 'Acme',
  task_name: 'ci',
  task_display_name: 'CI',
  name: 'nixpkgs#hello',
  value: 1234,
  unit: 'MB',
  worker: 'worker-1',
  worker_name: null,
};

const sampleBuild: ExpensiveBuild = {
  build_id: 'build-1',
  project: 'project-1',
  name: 'nixos.conf',
  build_time_ms: 5000,
  worker: 'worker-uuid-1',
  worker_name: 'builder-1',
};

const sampleResource: ExpensiveResource = {
  derivation: 'drv-1',
  project: 'project-1',
  name: 'hello',
  value: 512,
  unit: 'MB',
  worker: 'worker-uuid-1',
  worker_name: null,
};

const sampleTopProject: TopProjectBuildTime = {
  project: 'project-1',
  project_name: 'nixpkgs',
  project_display_name: 'Nixpkgs',
  total_build_ms: 60000,
  build_count: 3,
};

const sampleNode: FlakeGraphNode = {
  path: 'root.packages',
  parent: 'root',
  name: 'packages',
  kind: 'attrs',
  is_derivation: false,
  drv_path: null,
};

describe('BoardService', () => {
  let service: BoardService;
  let httpMock: HttpTestingController;

  beforeEach(() => {
    TestBed.configureTestingModule({
      providers: [BoardService, provideHttpClient(), provideHttpClientTesting()],
    });
    service = TestBed.inject(BoardService);
    httpMock = TestBed.inject(HttpTestingController);
  });

  afterEach(() => httpMock.verify());

  it('getExpensiveEvalsByResource() GETs the resource endpoint and unwraps the array', () => {
    let result: ExpensiveEval[] | undefined;
    service.getExpensiveEvalsByResource('rss').subscribe((v) => (result = v));

    const req = httpMock.expectOne(
      `${apiUrl}/board/evals/expensive-by-resource?metric=rss&window_days=30`
    );
    expect(req.request.method).toBe('GET');
    req.flush({ error: false, message: [sampleEval] });

    expect(result).toEqual([sampleEval]);
  });

  it('getExpensive() GETs the jobs endpoint with the window and keeps worker names', () => {
    let result: ExpensiveBuild[] | undefined;
    service.getExpensive(7).subscribe((v) => (result = v));

    const req = httpMock.expectOne(`${apiUrl}/board/jobs/expensive?window_days=7`);
    expect(req.request.method).toBe('GET');
    req.flush({ error: false, message: [sampleBuild] });

    expect(result).toEqual([sampleBuild]);
  });

  it('getExpensiveByResource() GETs the resource endpoint and unwraps the array', () => {
    let result: ExpensiveResource[] | undefined;
    service.getExpensiveByResource('ram').subscribe((v) => (result = v));

    const req = httpMock.expectOne(
      `${apiUrl}/board/jobs/expensive-by-resource?metric=ram&window_days=30`
    );
    expect(req.request.method).toBe('GET');
    req.flush({ error: false, message: [sampleResource] });

    expect(result).toEqual([sampleResource]);
  });

  it('getTopProjects() GETs the top-projects endpoint and keeps project names', () => {
    let result: TopProjectBuildTime[] | undefined;
    service.getTopProjects().subscribe((v) => (result = v));

    const req = httpMock.expectOne(`${apiUrl}/board/expensive/top-projects?window_days=30`);
    expect(req.request.method).toBe('GET');
    req.flush({ error: false, message: [sampleTopProject] });

    expect(result).toEqual([sampleTopProject]);
  });

  it('getEvalFlakeGraph() GETs the flake-graph endpoint and unwraps the array', () => {
    let result: FlakeGraphNode[] | undefined;
    service.getEvalFlakeGraph('eval-1').subscribe((v) => (result = v));

    const req = httpMock.expectOne(`${apiUrl}/evals/eval-1/flake-graph`);
    expect(req.request.method).toBe('GET');
    req.flush({ error: false, message: [sampleNode] });

    expect(result).toEqual([sampleNode]);
  });

  it('getScoringRules() unwraps the catalog and caches it across subscribers', () => {
    const rules: RuleDescription[] = [{ rule: 'WaitTimeRule', description: 'Grows with queue wait.' }];
    let first: RuleDescription[] | undefined;
    service.getScoringRules().subscribe((v) => (first = v));

    const req = httpMock.expectOne(`${apiUrl}/board/scoring/rules`);
    expect(req.request.method).toBe('GET');
    req.flush({ error: false, message: rules });
    expect(first).toEqual(rules);

    let second: RuleDescription[] | undefined;
    service.getScoringRules().subscribe((v) => (second = v));
    httpMock.expectNone(`${apiUrl}/board/scoring/rules`);
    expect(second).toEqual(rules);
  });
});
