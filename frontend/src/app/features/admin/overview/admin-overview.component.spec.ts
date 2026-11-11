/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { EMPTY, NEVER, type Observable, of, throwError } from 'rxjs';
import { AdminStatusTilesWithMaintenanceActionsComponent } from './admin-overview.component';
import { BoardService } from '@core/services/board.service';
import { AdminService } from '@core/services/admin.service';
import { NotifyService } from '@core/services/notify.service';
import { AuthService } from '@core/services/auth.service';
import { UserService } from '@core/services/user.service';
import { ConfirmService, type ConfirmRequest } from '@shared/confirm';
import { AdminStorageResponse, AdminTaskDto, AdminWorkerListEntry, BoardHealth, PendingJobsResponse } from '@core/models';

const HEALTH = { draining: false } as BoardHealth;

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

const LIVE = {
  id: 'w1', capabilities: {}, architectures: [], system_features: [], max_concurrent_builds: 4, assigned_job_count: 1, draining: false,
} as unknown as NonNullable<AdminWorkerListEntry['live']>;

const WORKERS: AdminWorkerListEntry[] = [
  { worker_id: 'w1', connected: true, registrations: [], projects: [], live: LIVE, requested_by_admin: false },
  { worker_id: 'w2', connected: true, registrations: [], projects: [], live: { ...LIVE, id: 'w2', draining: true }, requested_by_admin: true },
  { worker_id: 'w3', connected: false, registrations: [], projects: [], live: null, requested_by_admin: false },
];

const STORAGE: AdminStorageResponse = { disk_total_bytes: 4096, disk_free_bytes: 2048, caches: [], projects: [], tasks: [] };
const REQUEST_PROVIDERS = [
  { provide: AuthService, useValue: { isAuthenticated: () => false } },
  {
    provide: UserService,
    useValue: { getRequestsAwaitingUserDecision: () => of({ invites: [{}], subscription_requests: [{}, {}], team_requests: [{}] }) },
  },
];
const PENDING = { jobs: [{}, {}], other_pending: 3 } as unknown as PendingJobsResponse;

let confirm: ReturnType<typeof vi.fn>;
let startDeepGc: ReturnType<typeof vi.fn>;

function setup(
  health: BoardHealth = HEALTH,
  setDraining = vi.fn(() => of({ draining: true })),
  confirmed = true,
  tasks: AdminTaskDto[] = [TASK],
): ComponentFixture<AdminStatusTilesWithMaintenanceActionsComponent> {
  confirm = vi.fn((request: ConfirmRequest): Observable<unknown> => (confirmed ? request.action() : EMPTY));
  startDeepGc = vi.fn(() => of({ task_id: 't2', status: 'pending' }));
  TestBed.configureTestingModule({
    imports: [AdminStatusTilesWithMaintenanceActionsComponent],
    providers: [
      provideRouter([]),
      ...REQUEST_PROVIDERS,
      { provide: BoardService, useValue: { getHealth: () => of(health), getPendingJobs: () => of(PENDING) } },
      { provide: ConfirmService, useValue: { confirm } },
      {
        provide: AdminService,
        useValue: {
          listTasks: () => of(tasks),
          listWorkers: () => of(WORKERS),
          getDiskAndStorageUsageByCacheProjectAndTask: () => of(STORAGE),
          startDeepGc,
          setDraining,
        },
      },
    ],
  });
  const fixture = TestBed.createComponent(AdminStatusTilesWithMaintenanceActionsComponent);
  fixture.detectChanges();
  return fixture;
}

function actionButton(fixture: ComponentFixture<AdminStatusTilesWithMaintenanceActionsComponent>, label: string): HTMLButtonElement {
  return Array.from((fixture.nativeElement as HTMLElement).querySelectorAll<HTMLButtonElement>('.actions button'))
    .find((b) => b.textContent?.includes(label))!;
}

function tile(fixture: ComponentFixture<AdminStatusTilesWithMaintenanceActionsComponent>, label: string): HTMLElement {
  return Array.from((fixture.nativeElement as HTMLElement).querySelectorAll<HTMLElement>('.tile'))
    .find((t) => t.querySelector('.label')?.textContent === label)!;
}

describe('AdminStatusTilesWithMaintenanceActionsComponent', () => {
  afterEach(() => TestBed.resetTestingModule());

  describe('tiles', () => {
    it('counts pending jobs, including the ones not listed and links to the pending view', () => {
      const pending = tile(setup(), 'Pending Jobs');
      expect(pending.textContent).toContain('5');
      expect(pending.getAttribute('href')).toBe('/board/live?view=pending');
    });

    it('counts connected and draining workers and links both to the workers tab', () => {
      const fixture = setup();
      expect(tile(fixture, 'Connected workers').textContent).toContain('2 of 3');
      expect(tile(fixture, 'Draining workers').textContent).toContain('1');
      expect(tile(fixture, 'Draining workers').getAttribute('href')).toBe('/admin/workers');
    });

    it('shows free disk space linked to the storage tab', () => {
      const disk = tile(setup(), 'Free Disk Space');
      expect(disk.textContent).toContain('2.0 KiB');
      expect(disk.getAttribute('href')).toBe('/admin/storage');
    });

    it('counts only pending and running maintenance tasks', () => {
      const fixture = setup(HEALTH, undefined, true, [TASK, { ...TASK, id: 't2', status: 'completed' }, { ...TASK, id: 't3', status: 'pending' }]);
      expect(tile(fixture, 'Running Maintenance').textContent).toContain('2');
    });

    it('counts every request waiting for a decision and links to the Requests page', () => {
      const requests = tile(setup(), 'Pending Requests');
      expect(requests.textContent).toContain('4');
      expect(requests.getAttribute('href')).toBe('/settings/invites');
    });
  });

  it('reports a failed load and loads again on Try Again', () => {
    const listWorkers = vi.fn().mockReturnValueOnce(throwError(() => new Error('workers failed'))).mockReturnValue(of(WORKERS));
    TestBed.configureTestingModule({
      imports: [AdminStatusTilesWithMaintenanceActionsComponent],
      providers: [
        provideRouter([]),
        ...REQUEST_PROVIDERS,
        { provide: BoardService, useValue: { getHealth: () => of(HEALTH), getPendingJobs: () => of(PENDING) } },
        {
          provide: AdminService,
          useValue: { listTasks: () => of([]), listWorkers, getDiskAndStorageUsageByCacheProjectAndTask: () => of(STORAGE) },
        },
      ],
    });
    const fixture = TestBed.createComponent(AdminStatusTilesWithMaintenanceActionsComponent);
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.textContent).toContain('Failed to load admin overview');
    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')?.click();
    fixture.detectChanges();
    expect(listWorkers).toHaveBeenCalledTimes(2);
    expect(root.textContent).toContain('Maintenance');
  });

  describe('task table', () => {
    it('names the Deep GC task row in words', () => {
      expect(setup().nativeElement.textContent).toContain('Deep GC');
    });

    it('names the step a running round is on, without its storage shard', () => {
      const text = setup().nativeElement.textContent;
      expect(text).toContain('NARs');
      expect(text).not.toContain('0a');
    });

    it('leaves the step of a completed round empty', () => {
      const fixture = setup(HEALTH, undefined, true, [{ ...TASK, status: 'completed' }]);
      expect(fixture.nativeElement.textContent).not.toContain('NARs');
    });
  });

  describe('draining controls', () => {
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
      actionButton(fixture, 'Enable Draining').click();
      expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Enable Draining', target: 'All projects on this instance' }));
      expect(setDraining).toHaveBeenCalledWith(true);
    });

    it('sends nothing when enabling draining is cancelled', () => {
      const setDraining = vi.fn(() => of({ draining: true }));
      const fixture = setup(HEALTH, setDraining, false);
      actionButton(fixture, 'Enable Draining').click();
      expect(confirm).toHaveBeenCalledOnce();
      expect(setDraining).not.toHaveBeenCalled();
    });

    it('marks Disable Draining as busy while the request runs', () => {
      const fixture = setup({ ...HEALTH, draining: true }, vi.fn(() => NEVER));
      const button = actionButton(fixture, 'Disable Draining');
      button.click();
      fixture.detectChanges();
      expect(button.getAttribute('aria-busy')).toBe('true');
    });

    it('disables draining at once and reports a failure', () => {
      const refused = new Error('boom');
      const setDraining = vi.fn(() => throwError(() => refused));
      const fixture = setup({ ...HEALTH, draining: true }, setDraining);
      const failure = vi.spyOn(TestBed.inject(NotifyService), 'failure');
      actionButton(fixture, 'Disable Draining').click();
      expect(confirm).not.toHaveBeenCalled();
      expect(setDraining).toHaveBeenCalledWith(false);
      expect(failure).toHaveBeenCalledWith('Failed to disable draining', refused);
    });
  });

  describe('deep GC', () => {
    it('asks first, then starts a round', () => {
      const fixture = setup();
      const success = vi.spyOn(TestBed.inject(NotifyService), 'success');
      actionButton(fixture, 'Run Deep GC').click();
      expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Run Deep GC', target: 'All storage backends' }));
      expect(startDeepGc).toHaveBeenCalledOnce();
      expect(success).toHaveBeenCalledWith('Deep GC started');
    });

    it('sends nothing when the round is cancelled', () => {
      const fixture = setup(HEALTH, undefined, false);
      actionButton(fixture, 'Run Deep GC').click();
      expect(startDeepGc).not.toHaveBeenCalled();
    });
  });
});

describe('AdminStatusTilesWithMaintenanceActionsComponent - admin tasks', () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
    TestBed.resetTestingModule();
  });

  function setupWithTaskRequests(listTasks: ReturnType<typeof vi.fn>): ComponentFixture<AdminStatusTilesWithMaintenanceActionsComponent> {
    TestBed.configureTestingModule({
      imports: [AdminStatusTilesWithMaintenanceActionsComponent],
      providers: [
        provideRouter([]),
        ...REQUEST_PROVIDERS,
        { provide: BoardService, useValue: { getHealth: () => of(HEALTH), getPendingJobs: () => of(PENDING) } },
        {
          provide: AdminService,
          useValue: { listTasks, listWorkers: () => of(WORKERS), getDiskAndStorageUsageByCacheProjectAndTask: () => of(STORAGE) },
        },
      ],
    });
    const fixture = TestBed.createComponent(AdminStatusTilesWithMaintenanceActionsComponent);
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
    expect(root.textContent).toContain('Connected workers');

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
