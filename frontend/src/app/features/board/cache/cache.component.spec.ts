/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { Observable, Subject, finalize } from 'rxjs';
import { BoardCacheComponent } from './cache.component';
import { LiveService } from '@core/services/live.service';

function snapshot(displayName: string) {
  return {
    event: 'board.cache',
    at: '2026-10-30T12:00:00Z',
    content: {
      stats: { totals: {}, traffic: [], storage: [] },
      upstream: {
        upstream_caches: [
          {
            upstream_id: displayName,
            display_name: displayName,
            url: `https://${displayName}`,
            avg_latency_ms: 42,
            hit_rate: 0.87,
            requests_total: 10,
            latency: [],
            hit_rate_series: [],
          },
        ],
      },
    },
  };
}

function render() {
  const sockets: { path: string; frames: Subject<unknown>; closed: boolean }[] = [];
  const connect = vi.fn((path: string) => {
    const socket = { path, frames: new Subject<unknown>(), closed: false };
    sockets.push(socket);
    return socket.frames.pipe(finalize(() => (socket.closed = true))) as Observable<unknown>;
  });
  TestBed.configureTestingModule({
    imports: [BoardCacheComponent],
    providers: [provideRouter([]), { provide: LiveService, useValue: { connect } }],
  });
  const fixture = TestBed.createComponent(BoardCacheComponent);
  fixture.detectChanges();
  return { fixture, sockets, root: fixture.nativeElement as HTMLElement };
}

describe('BoardCacheComponent', () => {
  it('shows the spinner only until the first pushed snapshot, then swaps data in place', () => {
    const { fixture, sockets, root } = render();
    expect(root.querySelector('gr-loading-spinner')).not.toBeNull();

    sockets[0].frames.next(snapshot('cache.nixos.org'));
    fixture.detectChanges();
    expect(root.querySelector('gr-loading-spinner')).toBeNull();
    expect(root.textContent).toContain('cache.nixos.org');

    sockets[0].frames.next(snapshot('nix-community.cachix.org'));
    fixture.detectChanges();
    expect(root.querySelector('gr-loading-spinner')).toBeNull();
    expect(root.textContent).toContain('nix-community.cachix.org');
  });

  it('charts the busiest caches in the pushed order, by display name', () => {
    const { fixture, sockets, root } = render();
    const frame = snapshot('cache.nixos.org');
    frame.content.stats = {
      ...frame.content.stats,
      caches: [
        { name: 'main', display_name: 'Main', bytes: 900, requests: 9 },
        { name: 'dev', display_name: '', bytes: 100, requests: 1 },
      ],
    } as typeof frame.content.stats;
    sockets[0].frames.next(frame);
    fixture.detectChanges();

    const component = fixture.componentInstance;
    expect(component.busiestCats()).toEqual(['Main', 'dev']);
    expect(component.busiestSeries()[0].data).toEqual([900, 100]);
    expect(root.querySelector('.busiest-caches')).not.toBeNull();
  });

  it('subscribes to total traffic first and resubscribes for external traffic once picked', () => {
    const { fixture, sockets, root } = render();
    expect(sockets.map((s) => s.path)).toEqual(['/board/cache/live?window_hours=24&traffic=total']);
    sockets[0].frames.next(snapshot('cache.nixos.org'));
    fixture.detectChanges();

    const external = [...root.querySelectorAll('button')].find((b) => b.textContent?.trim() === 'External');
    external!.click();
    fixture.detectChanges();

    expect(sockets.map((s) => s.path)).toEqual([
      '/board/cache/live?window_hours=24&traffic=total',
      '/board/cache/live?window_hours=24&traffic=external',
    ]);
    expect(sockets[0].closed).toBe(true);
    expect(root.querySelector('gr-loading-spinner')).toBeNull();
  });
});
