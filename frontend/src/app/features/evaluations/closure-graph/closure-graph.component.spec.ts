/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { of, throwError } from 'rxjs';
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
    getEvaluation: vi.fn(() => of({ commit: 'abcdef1234567890', task_name: 'ci', task_display_name: 'CI checks' })),
    getBuild: vi.fn(() => of({ id: 'x1', evaluation: 'e9' })),
  };
}

const crumbs = { buildPage: vi.fn(() => []), buildEvaluationPageBreadcrumbs: vi.fn(() => []) };

function setup(kind: string, type?: string, graph: ClosureGraph = emptyGraph, svc = service(graph)) {
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
      { provide: BreadcrumbsService, useValue: crumbs },
      { provide: ActivatedRoute, useValue: route },
      { provide: EvaluationsService, useValue: svc },
    ],
  });
  const fixture = TestBed.createComponent(ClosureGraphComponent);
  fixture.detectChanges();
  return Object.assign(svc, { fixture });
}

describe('ClosureGraphComponent - breadcrumb', () => {
  it('leads a build closure opened without an origin back through the evaluation of the build', () => {
    setup('build');
    expect(crumbs.buildPage).toHaveBeenLastCalledWith('acme', { task: null, evaluation: 'e9' }, 'x1', 'Closure');
  });

  it('ends an evaluation closure under the evaluation itself', () => {
    const svc = setup('eval');
    expect(svc.getBuild).not.toHaveBeenCalled();
    expect(crumbs.buildEvaluationPageBreadcrumbs).toHaveBeenLastCalledWith('acme', 'x1', null, { label: 'Closure' });
  });
});

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
    const rect = root.querySelector('svg rect')!;
    for (const attribute of ['x', 'y', 'width', 'height']) {
      expect(Number(rect.getAttribute(attribute))).not.toBeNaN();
    }
    expect(Number(rect.getAttribute('width'))).toBeGreaterThan(0);
    expect(Number(root.querySelector('svg text')?.getAttribute('x'))).not.toBeNaN();
    expect(root.querySelector('.header-meta')?.textContent).toMatch(/1 package in closure\s*·\s*120 B/);
    fixture.destroy();
  });

  it('titles an evaluation closure by its task, not by one of its roots or the commit', () => {
    const { fixture } = setup('eval', undefined, {
      ...emptyGraph, roots: ['deployment', 'docs'], total_size_bytes: 120, node_count: 2,
      nodes: [{ id: 'deployment', name: 'deployment', path: '/nix/store/hash-deployment', nar_size: 60 }, { id: 'docs', name: 'docs', path: '/nix/store/hash-docs', nar_size: 60 }],
    });
    fixture.detectChanges();
    expect((fixture.nativeElement as HTMLElement).querySelector('h1')?.textContent?.trim()).toBe('CI checks');
    fixture.destroy();
  });

  it('keeps the breadcrumb on a failed load and loads again on Try Again', () => {
    const svc = service(emptyGraph);
    svc.getBuildRuntimeClosure.mockReturnValueOnce(throwError(() => ({ status: 500, message: 'database unavailable' })));
    const { fixture } = setup('build', undefined, emptyGraph, svc);
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('gr-breadcrumb')).not.toBeNull();
    expect(root.textContent).toContain('database unavailable');

    [...root.querySelectorAll('button')].find((b) => b.textContent?.includes('Try Again'))!.click();
    fixture.detectChanges();
    expect(svc.getBuildRuntimeClosure).toHaveBeenCalledTimes(2);
    expect(root.querySelector('gr-empty-state')?.textContent).toContain('No outputs to measure');
  });

  it('explains an empty closure instead of drawing an empty canvas', () => {
    const root = setup('build').fixture.nativeElement as HTMLElement;
    expect(root.querySelector('svg.sankey-svg')).toBeNull();
    expect(root.querySelector('gr-empty-state')?.textContent).toContain('No outputs to measure');
    expect(root.textContent).not.toContain('0 packages in closure');
  });
});
