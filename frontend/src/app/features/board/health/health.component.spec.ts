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
  draining: false,
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

function setup(
  githubAppConfigured: () => Observable<boolean>,
  health: BoardHealth = HEALTH,
  setDraining = vi.fn(() => of({ draining: true })),
): ComponentFixture<BoardHealthComponent> {
  TestBed.configureTestingModule({
    imports: [BoardHealthComponent],
    providers: [
      provideRouter([]),
      { provide: BoardService, useValue: { getHealth: () => of(health) } },
      {
        provide: AdminService,
        useValue: {
          listTasks: () => of([TASK]),
          githubAppConfigured,
          startDeepGc: () => of({ task_id: 't2', status: 'pending' }),
          setDraining,
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

    it('hides the "Set up GitHub App" link', () => {
      const link: HTMLAnchorElement = fixture.nativeElement.querySelector('a.btn');
      expect(link).toBeNull();
      expect(fixture.nativeElement.textContent).not.toContain('Set up GitHub App');
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

  describe('draining controls', () => {
    afterEach(() => TestBed.resetTestingModule());

    it('offers "Enable Draining" and no banner when not draining', () => {
      const fixture = setup(() => of(true));
      expect(fixture.nativeElement.textContent).toContain('Enable Draining');
      expect(fixture.nativeElement.textContent).not.toContain('Instance is draining');
    });

    it('offers "Disable Draining" and shows a banner when draining', () => {
      const fixture = setup(() => of(true), { ...HEALTH, draining: true });
      expect(fixture.nativeElement.textContent).toContain('Disable Draining');
      expect(fixture.nativeElement.textContent).toContain('Instance is draining');
    });

    it('toggles draining via the admin service when clicked', () => {
      const setDraining = vi.fn(() => of({ draining: true }));
      const fixture = setup(() => of(true), HEALTH, setDraining);
      const button: HTMLButtonElement = Array.from(
        fixture.nativeElement.querySelectorAll('button.btn'),
      ).find((b) => (b as HTMLButtonElement).textContent?.includes('Enable Draining')) as HTMLButtonElement;
      button.click();
      expect(setDraining).toHaveBeenCalledWith(true);
    });
  });
});
