/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { of } from 'rxjs';
import { ClosureGraphComponent } from './closure-graph.component';
import { EvaluationsService } from '@core/services/evaluations.service';
import { ClosureGraph } from '@core/models';

const emptyGraph: ClosureGraph = {
  roots: [], total_size_bytes: 0, node_count: 0, edge_count: 0,
  truncated: false, nodes: [], edges: [],
};

function service(graph: ClosureGraph) {
  return {
    getBuildClosure: vi.fn(() => of(graph)),
    getEvalClosure: vi.fn(() => of(graph)),
    getBuildRuntimeClosure: vi.fn(() => of(graph)),
    getEvalRuntimeClosure: vi.fn(() => of(graph)),
  };
}

function setup(kind: string, type?: string, graph: ClosureGraph = emptyGraph) {
  const svc = service(graph);
  const route = {
    snapshot: {
      paramMap: convertToParamMap({ project: 'acme', kind, id: 'x1' }),
      queryParamMap: convertToParamMap(type ? { type } : {}),
    },
  } as unknown as ActivatedRoute;

  TestBed.configureTestingModule({
    imports: [ClosureGraphComponent],
    providers: [
      provideRouter([]),
      { provide: BreadcrumbsService, useValue: { buildPage: () => [] } },
      { provide: ActivatedRoute, useValue: route },
      { provide: EvaluationsService, useValue: svc },
    ],
  });
  const fixture = TestBed.createComponent(ClosureGraphComponent);
  fixture.detectChanges();
  return Object.assign(svc, { fixture });
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

  it('heads the page with one h1', () => {
    expect((setup('build').fixture.nativeElement as HTMLElement).querySelectorAll('h1').length).toBe(1);
  });
});

describe('ClosureGraphComponent - diagram', () => {
  it('Draw the closure of one package at its own size and count it in singular form.', async () => {
    const { fixture } = setup('build', undefined, {
      ...emptyGraph, roots: ['ok'], total_size_bytes: 120, node_count: 1,
      nodes: [{ id: 'ok', name: 'ok', path: '/nix/store/hash-ok', nar_size: 120 }],
    });
    const root = fixture.nativeElement as HTMLElement;

    await vi.waitFor(() => expect(root.querySelector('svg text')?.textContent).toBe('ok · 120 B'), { timeout: 5000 });
    expect(root.querySelector('svg rect')?.getAttribute('height')).not.toContain('NaN');
    expect(root.querySelector('.header-meta')?.textContent).toContain('1 package in closure');
    fixture.destroy();
  });
});
