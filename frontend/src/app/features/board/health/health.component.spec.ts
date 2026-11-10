/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { EMPTY, NEVER, type Observable, of, throwError } from 'rxjs';
import { BoardHealthComponent } from './health.component';
import { BoardService } from '@core/services/board.service';
import { AdminService } from '@core/services/admin.service';
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
  health: BoardHealth = HEALTH,
  setDraining = vi.fn(() => of({ draining: true })),
  confirmed = true,
  tasks: AdminTaskDto[] = [TASK],
): ComponentFixture<BoardHealthComponent> {
  confirm = vi.fn((request: ConfirmRequest): Observable<unknown> => (confirmed ? request.action() : EMPTY));
  startDeepGc = vi.fn(() => of({ task_id: 't2', status: 'pending' }));
  TestBed.configureTestingModule({
    imports: [BoardHealthComponent],
    providers: [
      provideRouter([]),
      { provide: BoardService, useValue: { getHealth: () => of(health) } },
      { provide: ConfirmService, useValue: { confirm } },
      {
        provide: AdminService,
        useValue: {
          listTasks: () => of(tasks),
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
  return Array.from((fixture.nativeElement as HTMLElement).querySelectorAll<HTMLButtonElement>('.admin-actions button'))
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

    it('Name the Deep GC task row in words', () => {
      expect(fixture.nativeElement.textContent).toContain('Deep GC');
    });

    it('names the step a running round is on, without its storage shard', () => {
      expect(fixture.nativeElement.textContent).toContain('NARs');
      expect(fixture.nativeElement.textContent).not.toContain('0a');
    });

    it('keeps only the maintenance actions, leaving the user-menu links to the user menu', () => {
      const actions = (fixture.nativeElement as HTMLElement).querySelector('.admin-actions') as HTMLElement;
      expect(actions.querySelectorAll('a').length).toBe(0);
      expect(actions.textContent).not.toContain('Set up GitHub App');
      expect(actions.textContent).not.toContain('Event notifications');
    });
  });

  describe('draining controls', () => {
    afterEach(() => TestBed.resetTestingModule());

    it('offers "Enable Draining" and no banner when not draining', () => {
      const fixture = setup();
      expect(fixture.nativeElement.textContent).toContain('Enable Draining');
      expect(fixture.nativeElement.textContent).not.toContain('Instance is draining');
    });

    it('offers "Disable Draining" and shows a banner when draining', () => {
      const fixture = setup({ ...HEALTH, draining: true });
      expect(fixture.nativeElement.textContent).toContain('Disable Draining');
      expect(fixture.nativeElement.textContent).toContain('Instance is draining');
    });

    it('asks first, naming all projects, before it enables draining', () => {
      const setDraining = vi.fn(() => of({ draining: true }));
      const fixture = setup(HEALTH, setDraining);
      adminButton(fixture, 'Enable Draining').click();
      expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Enable Draining', target: 'All projects on this instance' }));
      expect(setDraining).toHaveBeenCalledWith(true);
    });

    it('sends nothing when enabling draining is cancelled', () => {
      const setDraining = vi.fn(() => of({ draining: true }));
      const fixture = setup(HEALTH, setDraining, false);
      adminButton(fixture, 'Enable Draining').click();
      expect(confirm).toHaveBeenCalledOnce();
      expect(setDraining).not.toHaveBeenCalled();
    });

    it('marks Disable Draining as busy while the request runs', () => {
      const fixture = setup({ ...HEALTH, draining: true }, vi.fn(() => NEVER));
      const button = adminButton(fixture, 'Disable Draining');
      button.click();
      fixture.detectChanges();
      expect(button.getAttribute('aria-busy')).toBe('true');
    });

    it('disables draining at once and reports a failure', () => {
      const refused = new Error('boom');
      const setDraining = vi.fn(() => throwError(() => refused));
      const fixture = setup({ ...HEALTH, draining: true }, setDraining);
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
      const fixture = setup();
      const success = vi.spyOn(TestBed.inject(NotifyService), 'success');
      adminButton(fixture, 'Run Deep GC').click();
      expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Run Deep GC', target: 'All storage backends' }));
      expect(startDeepGc).toHaveBeenCalledOnce();
      expect(success).toHaveBeenCalledWith('Deep GC started');
    });

    it('sends nothing when the round is cancelled', () => {
      const fixture = setup(HEALTH, undefined, false);
      adminButton(fixture, 'Run Deep GC').click();
      expect(startDeepGc).not.toHaveBeenCalled();
    });
  });

  it('leaves the step of a completed round empty', () => {
    const fixture = setup(HEALTH, undefined, true, [{ ...TASK, status: 'completed' }]);
    expect(fixture.nativeElement.textContent).not.toContain('NARs');
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

describe('BoardHealthComponent - admin tasks', () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
    TestBed.resetTestingModule();
  });

  function setupWithTaskRequests(listTasks: ReturnType<typeof vi.fn>): ComponentFixture<BoardHealthComponent> {
    TestBed.configureTestingModule({
      imports: [BoardHealthComponent],
      providers: [
        provideRouter([]),
        { provide: BoardService, useValue: { getHealth: () => of(HEALTH) } },
        { provide: AdminService, useValue: { listTasks } },
      ],
    });
    const fixture = TestBed.createComponent(BoardHealthComponent);
    fixture.detectChanges();
    return fixture;
  }

  it('shows a failed task list request with Try Again instead of an empty list', () => {
    const listTasks = vi.fn()
      .mockReturnValueOnce(throwError(() => new Error('tasks endpoint failed')))
      .mockReturnValue(of([TASK]));
    const fixture = setupWithTaskRequests(listTasks);
    const root = fixture.nativeElement as HTMLElement;
    expect(root.textContent).toContain('Failed to load admin tasks');
    expect(root.textContent).not.toContain('No admin tasks yet.');

    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')?.click();
    fixture.detectChanges();
    expect(listTasks).toHaveBeenCalledTimes(2);
    expect(root.textContent).toContain('Deep GC');
  });

  it('reloads the task list on a timer only while a task is still running', () => {
    vi.useFakeTimers();
    vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('visible');
    const listTasks = vi.fn()
      .mockReturnValueOnce(of([TASK]))
      .mockReturnValue(of([{ ...TASK, status: 'completed' }]));
    setupWithTaskRequests(listTasks);

    vi.advanceTimersByTime(5_000);
    expect(listTasks).toHaveBeenCalledTimes(2);
    vi.advanceTimersByTime(20_000);
    expect(listTasks).toHaveBeenCalledTimes(2);
  });
});
