/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { ActivatedRoute, Router, convertToParamMap, provideRouter } from '@angular/router';
import { Observable, Subject, of, throwError } from 'rxjs';
import { EvaluationsService } from '@core/services/evaluations.service';
import { LiveEvent, LiveService } from '@core/services/live.service';
import { DependencyGraphComponent } from './dependency-graph.component';
import { BuildGraph } from '@core/models';

const node = (id: string, status: string | null) => ({
  id, build: id, name: id, path: `/nix/store/hash-${id}.drv`, status,
  build_time_ms: null, build_started_at: null,
});

describe('DependencyGraphComponent live updates', () => {
  afterEach(() => vi.useRealTimers());

  function setup(graph: BuildGraph, query: Record<string, string> = {}, firstLoad?: Observable<BuildGraph>) {
    vi.useFakeTimers();
    const frames = new Subject<LiveEvent>();
    const getBuildGraph = vi.fn(() => of(graph));
    if (firstLoad) getBuildGraph.mockReturnValueOnce(firstLoad);
    TestBed.configureTestingModule({
      imports: [DependencyGraphComponent],
      providers: [
        provideRouter([]),
      { provide: BreadcrumbsService, useValue: { buildPage: () => [] } },
        { provide: ActivatedRoute, useValue: {
          snapshot: {
            paramMap: convertToParamMap({ project: 'proj', buildId: 'b1' }),
            queryParamMap: convertToParamMap(query),
          },
        } },
        { provide: EvaluationsService, useValue: { getBuildGraph, getBuild: () => of({ id: 'b1', evaluation: 'e9' }) } },
        { provide: LiveService, useValue: { connect: () => frames } },
      ],
    });
    const fixture = TestBed.createComponent(DependencyGraphComponent);
    fixture.detectChanges();
    vi.advanceTimersByTime(0);
    const send = (event: string) => {
      frames.next({ event, at: '', content: {} });
      vi.advanceTimersByTime(300);
    };
    return { fixture, getBuildGraph, send };
  }

  it('reloads the graph on a status change but not on transfer progress', () => {
    const { fixture, getBuildGraph, send } = setup({ root: 'root', nodes: [node('root', 'Building')], edges: [] });

    expect(getBuildGraph).toHaveBeenCalledTimes(1);
    send('build.progress');
    expect(getBuildGraph).toHaveBeenCalledTimes(1);
    send('build.status_changed');
    expect(getBuildGraph).toHaveBeenCalledTimes(2);
    fixture.destroy();
  });

  it('stops following once only derivations without a build are left unfinished', () => {
    const { fixture, getBuildGraph, send } = setup({
      root: 'root',
      nodes: [node('root', 'Completed'), node('input', null)],
      edges: [{ source: 'input', target: 'root' }],
    });

    send('build.status_changed');
    expect(getBuildGraph).toHaveBeenCalledTimes(1);
    expect(fixture.nativeElement.textContent).toContain('No build');
    fixture.destroy();
  });

  it('counts in the singular and names statuses with readable labels', () => {
    const { fixture } = setup({
      root: 'root',
      nodes: [node('root', 'FailedPermanent'), node('lib', 'DependencyFailed')],
      edges: [{ source: 'lib', target: 'root' }],
    });
    fixture.detectChanges();
    const header = (fixture.nativeElement as HTMLElement).querySelector('.header-meta')?.textContent ?? '';
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';

    expect(header).toContain('2 builds');
    expect(header).toContain('1 link');
    expect(header).not.toContain('links');
    expect(text).toContain('Dependency failed');
    expect(text).not.toContain('FailedPermanent');
    fixture.destroy();
  });

  it('keeps the breadcrumb on a failed load and loads again on Try Again', () => {
    const { fixture, getBuildGraph } = setup(
      { root: 'root', nodes: [node('root', 'Completed')], edges: [] }, {},
      throwError(() => ({ status: 500, message: 'database unavailable' })),
    );
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('gr-breadcrumb')).not.toBeNull();
    expect(root.textContent).toContain('database unavailable');

    [...root.querySelectorAll('button')].find((b) => b.textContent?.includes('Try Again'))!.click();
    fixture.detectChanges();
    expect(getBuildGraph).toHaveBeenCalledTimes(2);
    expect(root.querySelector('.header-meta')?.textContent).toContain('1 build');
    fixture.destroy();
  });

  it('A click on a node can open the build log. A drag of the graph does not open the build log.', () => {
    const { fixture } = setup({ root: 'root', nodes: [node('root', 'Completed')], edges: [] }, { eval: 'e1' });
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    const cmp = fixture.componentInstance;
    const card = (fixture.nativeElement as HTMLElement).querySelector('svg g g') as SVGGElement;
    const at = (x: number) => ({ button: 0, clientX: x, clientY: 0 }) as MouseEvent;

    cmp.onSvgMousedown(at(0));
    cmp.onMousemove(at(40));
    cmp.onMouseup();
    card.dispatchEvent(new MouseEvent('click'));
    expect(navigate).not.toHaveBeenCalled();

    cmp.onSvgMousedown(at(0));
    cmp.onMouseup();
    card.dispatchEvent(new MouseEvent('click'));
    expect(navigate).toHaveBeenCalledTimes(1);
    fixture.destroy();
  });

  it('opens a wide graph at a readable zoom centred on the root instead of shrinking every card', () => {
    const rect = vi.spyOn(Element.prototype, 'getBoundingClientRect').mockReturnValue(new DOMRect(0, 0, 800, 600));
    const leaves = Array.from({ length: 12 }, (_, i) => node(`lib${i}`, 'Completed'));
    const wide = setup({
      root: 'root',
      nodes: [node('root', 'Completed'), ...leaves],
      edges: leaves.map((leaf) => ({ source: leaf.id, target: 'root' })),
    });
    expect(wide.fixture.componentInstance.scale()).toBe(0.6);
    wide.fixture.destroy();

    TestBed.resetTestingModule();
    const single = setup({ root: 'root', nodes: [node('root', 'Completed')], edges: [] });
    expect(single.fixture.componentInstance.scale()).toBeGreaterThan(0.6);
    single.fixture.destroy();
    rect.mockRestore();
  });

  it('opens the log from a focused node with Enter, also when the link carries no evaluation', () => {
    const { fixture } = setup({ root: 'root', nodes: [node('root', 'FailedPermanent')], edges: [] });
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    const card = (fixture.nativeElement as HTMLElement).querySelector('svg g g') as SVGGElement;

    expect(card.getAttribute('tabindex')).toBe('0');
    expect(card.hasAttribute('aria-label')).toBe(false);
    expect([...card.querySelectorAll('text')].map((text) => text.textContent)).toEqual(['root', 'Failed', '']);
    card.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter' }));
    expect(String(navigate.mock.calls[0][0])).toBe('/project/proj/log/e9?build=root');
    fixture.destroy();
  });
});
