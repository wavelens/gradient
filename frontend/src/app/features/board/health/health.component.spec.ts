/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { Observable, of } from 'rxjs';
import { BoardHealthComponent } from './health.component';
import { BoardService, BoardHealth } from '@core/services/board.service';
import { AdminService, AdminTask } from '@core/services/admin.service';

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
};

const TASK: AdminTask = {
  id: 't1',
  kind: 'deep_gc',
  status: 'completed',
  created_at: '2026-06-01T10:00:00',
  started_at: '2026-06-01T10:00:01',
  finished_at: '2026-06-01T10:05:00',
  progress: null,
  error: null,
  created_by: null,
};

function setup(githubAppConfigured: () => Observable<boolean>): ComponentFixture<BoardHealthComponent> {
  TestBed.configureTestingModule({
    imports: [BoardHealthComponent],
    providers: [
      provideRouter([]),
      { provide: BoardService, useValue: { getHealth: () => of(HEALTH) } },
      {
        provide: AdminService,
        useValue: {
          listTasks: () => of([TASK]),
          githubAppConfigured,
          startDeepGc: () => of({ task_id: 't2', status: 'pending' }),
        },
      },
    ],
  });
  const fixture = TestBed.createComponent(BoardHealthComponent);
  fixture.detectChanges();
  return fixture;
}

describe('BoardHealthComponent', () => {
  describe('when GitHub App is configured', () => {
    let fixture: ComponentFixture<BoardHealthComponent>;

    beforeEach(() => { fixture = setup(() => of(true)); });
    afterEach(() => TestBed.resetTestingModule());

    it('does not render "HTTP routes" heading', () => {
      expect(fixture.nativeElement.textContent).not.toContain('HTTP routes');
    });

    it('renders the deep_gc task row', () => {
      expect(fixture.nativeElement.textContent).toContain('deep_gc');
    });

    it('shows "GitHub App configured" text with disabled class on the link', () => {
      const link: HTMLAnchorElement = fixture.nativeElement.querySelector('a.btn');
      expect(link).toBeTruthy();
      expect(link.textContent?.trim()).toBe('GitHub App configured');
      expect(link.classList.contains('disabled')).toBe(true);
    });
  });

  describe('when GitHub App is NOT configured', () => {
    let fixture: ComponentFixture<BoardHealthComponent>;

    beforeEach(() => { fixture = setup(() => of(false)); });
    afterEach(() => TestBed.resetTestingModule());

    it('shows "Set up GitHub App" text without disabled class', () => {
      const link: HTMLAnchorElement = fixture.nativeElement.querySelector('a.btn');
      expect(link).toBeTruthy();
      expect(link.textContent?.trim()).toBe('Set up GitHub App');
      expect(link.classList.contains('disabled')).toBe(false);
    });
  });
});
