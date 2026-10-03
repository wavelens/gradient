/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { Subject, of } from 'rxjs';
import { BuildGraph, EvaluationsService } from '@core/services/evaluations.service';
import { LiveEvent, LiveService } from '@core/services/live.service';
import { DependencyGraphComponent } from './dependency-graph.component';

const node = (id: string, status: string | null) => ({
  id, build: id, name: id, path: `/nix/store/hash-${id}.drv`, status,
  created_at: '2026-01-01T00:00:00', updated_at: '2026-01-01T00:00:00',
});

describe('DependencyGraphComponent live updates', () => {
  afterEach(() => vi.useRealTimers());

  function setup(graph: BuildGraph) {
    vi.useFakeTimers();
    const frames = new Subject<LiveEvent>();
    const getBuildGraph = vi.fn(() => of(graph));
    TestBed.configureTestingModule({
      imports: [DependencyGraphComponent],
      providers: [
        provideRouter([]),
        { provide: ActivatedRoute, useValue: {
          snapshot: {
            paramMap: convertToParamMap({ project: 'proj', buildId: 'b1' }),
            queryParamMap: convertToParamMap({}),
          },
        } },
        { provide: EvaluationsService, useValue: { getBuildGraph } },
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
});
