/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { of } from 'rxjs';
import { ClosureGraphComponent } from './closure-graph.component';
import { EvaluationsService } from '@core/services/evaluations.service';

const emptyGraph = {
  roots: [], total_size_bytes: 0, node_count: 0, edge_count: 0,
  truncated: false, nodes: [], edges: [],
};

function service() {
  return {
    getBuildClosure: vi.fn(() => of(emptyGraph)),
    getEvalClosure: vi.fn(() => of(emptyGraph)),
    getBuildRuntimeClosure: vi.fn(() => of(emptyGraph)),
    getEvalRuntimeClosure: vi.fn(() => of(emptyGraph)),
  };
}

function setup(kind: string, type?: string) {
  const svc = service();
  const route = {
    snapshot: {
      paramMap: convertToParamMap({ org: 'acme', kind, id: 'x1' }),
      queryParamMap: convertToParamMap(type ? { type } : {}),
    },
  } as unknown as ActivatedRoute;

  TestBed.configureTestingModule({
    imports: [ClosureGraphComponent],
    providers: [
      provideRouter([]),
      { provide: ActivatedRoute, useValue: route },
      { provide: EvaluationsService, useValue: svc },
    ],
  });
  const fixture = TestBed.createComponent(ClosureGraphComponent);
  fixture.detectChanges();
  return svc;
}

describe('ClosureGraphComponent - view selection', () => {
  it('defaults to the runtime closure when no type query param is present', () => {
    const svc = setup('build');
    expect(svc.getBuildRuntimeClosure).toHaveBeenCalledWith('x1');
    expect(svc.getBuildClosure).not.toHaveBeenCalled();
  });

  it('shows the build closure when type=build', () => {
    const svc = setup('build', 'build');
    expect(svc.getBuildClosure).toHaveBeenCalledWith('x1');
    expect(svc.getBuildRuntimeClosure).not.toHaveBeenCalled();
  });

  it('uses the eval runtime closure for the eval scope by default', () => {
    const svc = setup('eval');
    expect(svc.getEvalRuntimeClosure).toHaveBeenCalledWith('x1');
  });
});
