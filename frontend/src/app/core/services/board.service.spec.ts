/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { HttpTestingController } from '@angular/common/http/testing';
import { BoardService, ExpensiveEval, FlakeGraphNode, RuleDescription } from './board.service';
import { environment } from '@environments/environment';

const apiUrl = environment.apiUrl;

const sampleEval: ExpensiveEval = {
  evaluation: 'eval-1',
  organization: 'org-1',
  name: 'nixpkgs#hello',
  value: 1234,
  unit: 'MB',
  worker: 'worker-1',
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
