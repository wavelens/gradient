/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { NEVER, of } from 'rxjs';
import { BoardCacheComponent } from './cache.component';
import { BoardService } from '@core/services/board.service';
import { LiveService } from '@core/services/live.service';

function render(upstreamCaches: unknown[] = []) {
  const board = {
    getCache: vi.fn(() => of({ totals: {}, traffic: [], storage: [] })),
    getUpstreamCacheStats: () => of({ upstream_caches: upstreamCaches }),
  };
  TestBed.configureTestingModule({
    imports: [BoardCacheComponent],
    providers: [
      provideRouter([]),
      { provide: BoardService, useValue: board },
      { provide: LiveService, useValue: { connect: () => NEVER } },
    ],
  });
  const fixture = TestBed.createComponent(BoardCacheComponent);
  fixture.detectChanges();
  return { fixture, board, root: fixture.nativeElement as HTMLElement };
}

describe('BoardCacheComponent', () => {
  it('renders an upstream cache row from getUpstreamCacheStats', () => {
    const { root } = render([
      {
        upstream_id: 'u1',
        display_name: 'cache.nixos.org',
        url: 'https://cache.nixos.org',
        avg_latency_ms: 42,
        hit_rate: 0.87,
        requests_total: 10,
        latency: [],
        hit_rate_series: [],
      },
    ]);
    expect(root.textContent).toContain('cache.nixos.org');
  });

  it('loads total traffic first and external traffic once picked', () => {
    const { fixture, board, root } = render();
    expect(board.getCache).toHaveBeenLastCalledWith(24, 'total');
    const external = [...root.querySelectorAll('button')].find((b) => b.textContent?.trim() === 'External');
    external!.click();
    fixture.detectChanges();
    expect(board.getCache).toHaveBeenLastCalledWith(24, 'external');
  });
});
