/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { of } from 'rxjs';
import { BoardCacheComponent } from './cache.component';
import { BoardService } from '@core/services/board.service';
import { LiveService } from '@core/services/live.service';

describe('BoardCacheComponent upstreams', () => {
  it('renders an upstream row from getUpstreams', () => {
    const board = {
      getCache: () => of({ totals: {}, traffic: [], storage: [] }),
      getUpstreams: () =>
        of({
          upstreams: [
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
          ],
        }),
    };
    const live = { connect: () => of(null) };

    TestBed.configureTestingModule({
      imports: [BoardCacheComponent],
      providers: [
        { provide: BoardService, useValue: board },
        { provide: LiveService, useValue: live },
      ],
    });

    const fixture = TestBed.createComponent(BoardCacheComponent);
    fixture.detectChanges();
    const text: string = fixture.nativeElement.textContent;
    expect(text).toContain('cache.nixos.org');
  });
});
