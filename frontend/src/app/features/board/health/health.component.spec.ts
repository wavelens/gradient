/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { EMPTY, type Observable, of, throwError } from 'rxjs';
import { BoardHealthComponent } from './health.component';
import { BoardService } from '@core/services/board.service';
import { AdminService } from '@core/services/admin.service';
import { ConfigService } from '@core/services/config.service';
import { NotifyService } from '@core/services/notify.service';
import { ConfirmService, type ConfirmRequest } from '@shared/confirm';
import { BoardHealth, AdminTaskDto } from '@core/models';

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

const TASK: AdminTaskDto = {
  id: 't1',
  kind: 'deep_gc',
  status: 'running',
  created_at: '2026-06-01T10:00:00',
  started_at: '2026-06-01T10:00:01',
  finished_at: '2026-06-01T10:05:00',
  progress: null,
  checkpoint: 'nars/0a',
  error: null,
  created_by: null,
};

let confirm: ReturnType<typeof vi.fn>;
let startDeepGc: ReturnType<typeof vi.fn>;

function setup(
  githubAppEnabled: boolean,
  health: BoardHealth = HEALTH,
  setDraining = vi.fn(() => of({ draining: true })),
  confirmed = true,
): ComponentFixture<BoardHealthComponent> {
  confirm = vi.fn((request: ConfirmRequest): Observable<unknown> => (confirmed ? request.action() : EMPTY));
  startDeepGc = vi.fn(() => of({ task_id: 't2', status: 'pending' }));
  TestBed.configureTestingModule({
    imports: [BoardHealthComponent],
    providers: [
      provideRouter([]),
      { provide: BoardService, useValue: { getHealth: () => of(health) } },
      { provide: ConfigService, useValue: { githubAppEnabled } },
      { provide: ConfirmService, useValue: { confirm } },
      {
        provide: AdminService,
        useValue: {
          listTasks: () => of([TASK]),
          startDeepGc,
          setDraining,
        },
      },
    ],
  });
  const fixture = TestBed.createComponent(BoardHealthComponent);
  fixture.detectChanges();
  return fixture;
}

function adminButton(fixture: ComponentFixture<BoardHealthComponent>, label: string): HTMLButtonElement {
  return Array.from((fixture.nativeElement as HTMLElement).querySelectorAll<HTMLButtonElement>('button.btn'))
    .find((b) => b.textContent?.includes(label))!;
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
        { provide: ConfigService, useValue: { githubAppEnabled: true } },
        { provide: AdminService, useValue: { listTasks: () => of([]) } },
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

  describe('when GitHub App is configured', () => {
    let fixture: ComponentFixture<BoardHealthComponent>;

    beforeEach(() => { fixture = setup(true); });
    afterEach(() => TestBed.resetTestingModule());

    it('does not render "HTTP routes" heading', () => {
      expect(fixture.nativeElement.textContent).not.toContain('HTTP routes');
    });

    it('renders the deep_gc task row', () => {
      expect(fixture.nativeElement.textContent).toContain('deep_gc');
    });

    it('shows where a running round resumes', () => {
      expect(fixture.nativeElement.textContent).toContain('nars/0a');
    });

    it('hides the "Set up GitHub App" link', () => {
      const link: HTMLAnchorElement = fixture.nativeElement.querySelector('a.btn[href="/admin/github-app"]');
      expect(link).toBeNull();
      expect(fixture.nativeElement.textContent).not.toContain('Set up GitHub App');
    });
  });

  describe('when GitHub App is NOT configured', () => {
    let fixture: ComponentFixture<BoardHealthComponent>;

    beforeEach(() => { fixture = setup(false); });
    afterEach(() => TestBed.resetTestingModule());

    it('shows "Set up GitHub App" text without disabled class', () => {
      const link: HTMLAnchorElement = fixture.nativeElement.querySelector('a.btn[href="/admin/github-app"]');
      expect(link).toBeTruthy();
      expect(link.textContent?.trim()).toBe('Set up GitHub App');
      expect(link.classList.contains('disabled')).toBe(false);
    });
  });

  describe('draining controls', () => {
    afterEach(() => TestBed.resetTestingModule());

    it('offers "Enable Draining" and no banner when not draining', () => {
      const fixture = setup(true);
      expect(fixture.nativeElement.textContent).toContain('Enable Draining');
      expect(fixture.nativeElement.textContent).not.toContain('Instance is draining');
    });

    it('offers "Disable Draining" and shows a banner when draining', () => {
      const fixture = setup(true, { ...HEALTH, draining: true });
      expect(fixture.nativeElement.textContent).toContain('Disable Draining');
      expect(fixture.nativeElement.textContent).toContain('Instance is draining');
    });

    it('asks first, naming all projects, before it enables draining', () => {
      const setDraining = vi.fn(() => of({ draining: true }));
      const fixture = setup(true, HEALTH, setDraining);
      adminButton(fixture, 'Enable Draining').click();
      expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Enable Draining', target: 'All projects on this instance' }));
      expect(setDraining).toHaveBeenCalledWith(true);
    });

    it('sends nothing when enabling draining is cancelled', () => {
      const setDraining = vi.fn(() => of({ draining: true }));
      const fixture = setup(true, HEALTH, setDraining, false);
      adminButton(fixture, 'Enable Draining').click();
      expect(confirm).toHaveBeenCalledOnce();
      expect(setDraining).not.toHaveBeenCalled();
    });

    it('disables draining at once and reports a failure', () => {
      const refused = new Error('boom');
      const setDraining = vi.fn(() => throwError(() => refused));
      const fixture = setup(true, { ...HEALTH, draining: true }, setDraining);
      const failure = vi.spyOn(TestBed.inject(NotifyService), 'failure');
      adminButton(fixture, 'Disable Draining').click();
      expect(confirm).not.toHaveBeenCalled();
      expect(setDraining).toHaveBeenCalledWith(false);
      expect(failure).toHaveBeenCalledWith('Failed to disable draining', refused);
    });
  });

  describe('deep GC', () => {
    afterEach(() => TestBed.resetTestingModule());

    it('asks first, then starts a round and reloads the tasks', () => {
      const fixture = setup(true);
      const success = vi.spyOn(TestBed.inject(NotifyService), 'success');
      adminButton(fixture, 'Run Deep GC').click();
      expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Run Deep GC', target: 'All storage backends' }));
      expect(startDeepGc).toHaveBeenCalledOnce();
      expect(success).toHaveBeenCalledWith('Deep GC started');
    });

    it('sends nothing when the round is cancelled', () => {
      const fixture = setup(true, HEALTH, undefined, false);
      adminButton(fixture, 'Run Deep GC').click();
      expect(startDeepGc).not.toHaveBeenCalled();
    });
  });

  it('lists every supervised loop and flags restarts', () => {
    const fixture = setup(true);
    const rows = fixture.nativeElement.querySelectorAll('gr-table.supervision tbody tr');
    expect(rows.length).toBe(2);
    expect(rows[0].textContent).toContain('build-dispatch');
    expect(rows[0].querySelector('td.bad')?.textContent?.trim()).toBe('1');
    expect(rows[1].textContent).toContain('never');
    TestBed.resetTestingModule();
  });
});
