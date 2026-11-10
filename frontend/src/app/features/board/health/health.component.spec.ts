/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { signal } from '@angular/core';
import { of, throwError } from 'rxjs';
import { BoardHealthComponent } from './health.component';
import { BoardService } from '@core/services/board.service';
import { AuthService } from '@core/services/auth.service';
import { BoardHealth } from '@core/models';

const HEALTH: BoardHealth = {
  version: '1.0.0',
  uptime_seconds: 3600,
  workers_connected: 2,
  jobs_pending: 1,
  jobs_active: 3,
  cache_bytes: 1024 * 1024 * 1024,
  cache_packages: 42,
  process: {
    resident_memory_bytes: 64 * 1024 * 1024,
    virtual_memory_bytes: 128 * 1024 * 1024,
    open_fds: 10,
    max_fds: 1024,
    threads: 4,
    cpu_seconds_total: 300,
  },
  http: [],
  rollup_lag_seconds: 0,
  latest_rollup_bucket: null,
  draining: false,
  supervised: [
    { name: 'build-dispatch', restarts: 1, pass_errors: 0, pass_timeouts: 0, last_ok_seconds_ago: 4, last_error: 'boom' },
    { name: 'retention', restarts: 0, pass_errors: 0, pass_timeouts: 0, last_ok_seconds_ago: null, last_error: null },
  ],
  proto_sessions: 2,
  unconfirmed_nars: 0,
  pending_deliveries: 0,
  failed_deliveries: 0,
  hot_nar_cache: { entries: 0, bytes: 0, hits: 0, misses: 0, evictions: 0 },
};

function setup(health: BoardHealth = HEALTH, superuser = false): ComponentFixture<BoardHealthComponent> {
  TestBed.configureTestingModule({
    imports: [BoardHealthComponent],
    providers: [
      provideRouter([]),
      { provide: BoardService, useValue: { getHealth: () => of(health) } },
      { provide: AuthService, useValue: { user: signal({ superuser }) } },
    ],
  });
  const fixture = TestBed.createComponent(BoardHealthComponent);
  fixture.detectChanges();
  return fixture;
}

describe('BoardHealthComponent', () => {
  it('replaces the endless spinner with the reason and loads again on Try Again', () => {
    const getHealth = vi.fn()
      .mockReturnValueOnce(throwError(() => new Error('health endpoint failed')))
      .mockReturnValue(of(HEALTH));
    TestBed.configureTestingModule({
      imports: [BoardHealthComponent],
      providers: [
        provideRouter([]),
        { provide: BoardService, useValue: { getHealth } },
        { provide: AuthService, useValue: { user: signal(null) } },
      ],
    });
    const fixture = TestBed.createComponent(BoardHealthComponent);
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.textContent).toContain('Failed to load health');
    expect(root.textContent).toContain('health endpoint failed');
    expect(root.textContent).not.toContain('Loading system health...');

    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')?.click();
    fixture.detectChanges();
    expect(getHealth).toHaveBeenCalledTimes(2);
    expect(root.textContent).toContain('build-dispatch');
    TestBed.resetTestingModule();
  });

  describe('details', () => {
    let fixture: ComponentFixture<BoardHealthComponent>;

    beforeEach(() => { fixture = setup(); });
    afterEach(() => TestBed.resetTestingModule());

    it('does not render "HTTP routes" heading', () => {
      expect(fixture.nativeElement.textContent).not.toContain('HTTP routes');
    });

    it('names connections and the notification outbox in plain words', () => {
      const text = fixture.nativeElement.textContent;
      expect(text).toContain('Worker and cache connections');
      expect(text).toContain('Notification outbox pending');
      expect(text).not.toContain('Sessions');
      expect(text).not.toContain('deliveries');
    });
  });

  it('points superusers to the Admin area for maintenance', () => {
    const fixture = setup(HEALTH, true);
    const link = (fixture.nativeElement as HTMLElement).querySelector<HTMLAnchorElement>('.admin-note a');
    expect(link?.getAttribute('href')).toBe('/admin/overview');
    expect(fixture.nativeElement.textContent).not.toContain('Run Deep GC');
    TestBed.resetTestingModule();
  });

  it('shows no Admin area note to other users', () => {
    const fixture = setup();
    expect((fixture.nativeElement as HTMLElement).querySelector('.admin-note')).toBeNull();
    TestBed.resetTestingModule();
  });

  it('lists only the supervised loops that restarted or failed', () => {
    const fixture = setup();
    const rows = fixture.nativeElement.querySelectorAll('gr-table.supervision tbody tr');
    expect(rows.length).toBe(1);
    expect(rows[0].textContent).toContain('build-dispatch');
    expect(rows[0].querySelector('td.bad')?.textContent?.trim()).toBe('1');
    TestBed.resetTestingModule();
  });

  it('sums up healthy loops in one line', () => {
    const healthy = { ...HEALTH, supervised: [HEALTH.supervised[1], { ...HEALTH.supervised[1], name: 'rollup' }] };
    const fixture = setup(healthy);
    const rows = fixture.nativeElement.querySelectorAll('gr-table.supervision tbody tr');
    expect(rows.length).toBe(1);
    expect(rows[0].textContent).toContain('All 2 supervised loops run with no restarts');
    TestBed.resetTestingModule();
  });
});
