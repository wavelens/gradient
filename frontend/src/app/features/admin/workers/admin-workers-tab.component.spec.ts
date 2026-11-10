/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { EMPTY, type Observable, of, throwError } from 'rxjs';
import { AdminWorkersTabComponent, getWorkerOwnerDisplayName } from './admin-workers-tab.component';
import { AdminService } from '@core/services/admin.service';
import { NotifyService } from '@core/services/notify.service';
import { ConfirmService, type ConfirmRequest } from '@shared/confirm';
import { AdminWorkerListEntry } from '@core/models';

const LIVE = {
  id: 'w1', capabilities: {}, architectures: [], system_features: [], max_concurrent_builds: 4, assigned_job_count: 1, draining: false,
} as unknown as NonNullable<AdminWorkerListEntry['live']>;
const PROJECT = { id: 'p1', name: 'web', display_name: 'Web' };
const TEAM = { id: 't1', name: 'ops', display_name: 'Ops', active: true, managed: false };

const IDLE: AdminWorkerListEntry = {
  worker_id: 'w1', display_name: 'Builder', connected: true, team: TEAM, registrations: [], projects: [PROJECT], live: LIVE, requested_by_admin: false,
};
const ADMIN_DRAINED: AdminWorkerListEntry = { ...IDLE, worker_id: 'w2', display_name: 'Admin drained', live: { ...LIVE, draining: true }, requested_by_admin: true };
const SELF_DRAINED: AdminWorkerListEntry = { ...IDLE, worker_id: 'w3', display_name: 'Self drained', live: { ...LIVE, draining: true } };
const OFFLINE: AdminWorkerListEntry = {
  ...IDLE, worker_id: 'w4', display_name: null, connected: false, team: null, live: null,
  registrations: [{ project: PROJECT, display_name: 'web worker', active: true, managed: false }],
};

let confirm: ReturnType<typeof vi.fn>;
let setWorkerDraining: ReturnType<typeof vi.fn>;

function setup(workers: AdminWorkerListEntry[], confirmed = true): ComponentFixture<AdminWorkersTabComponent> {
  confirm = vi.fn((request: ConfirmRequest): Observable<unknown> => (confirmed ? request.action() : EMPTY));
  setWorkerDraining = vi.fn((worker: string, draining: boolean) =>
    of({ ...workers.find((w) => w.worker_id === worker)!, requested_by_admin: draining }));
  TestBed.configureTestingModule({
    imports: [AdminWorkersTabComponent],
    providers: [
      { provide: ConfirmService, useValue: { confirm } },
      { provide: AdminService, useValue: { listWorkers: () => of(workers), setWorkerDraining } },
    ],
  });
  const fixture = TestBed.createComponent(AdminWorkersTabComponent);
  fixture.detectChanges();
  return fixture;
}

function row(fixture: ComponentFixture<AdminWorkersTabComponent>, name: string): HTMLTableRowElement {
  return Array.from((fixture.nativeElement as HTMLElement).querySelectorAll<HTMLTableRowElement>('tbody tr'))
    .find((r) => r.cells[0].textContent?.trim() === name)!;
}

function button(r: HTMLElement, label: string): HTMLButtonElement | undefined {
  return Array.from(r.querySelectorAll<HTMLButtonElement>('button')).find((b) => b.textContent?.includes(label));
}

describe('AdminWorkersTabComponent', () => {
  afterEach(() => TestBed.resetTestingModule());

  it('names the owning team, or the projects that registered the worker', () => {
    expect(getWorkerOwnerDisplayName(IDLE)).toBe('Ops (ops)');
    expect(getWorkerOwnerDisplayName(OFFLINE)).toBe('Web (web)');
  });

  it('shows status, live jobs and the worker id when no name is set', () => {
    const fixture = setup([IDLE, OFFLINE]);
    expect(row(fixture, 'Builder').textContent).toContain('Connected');
    expect(row(fixture, 'Builder').textContent).toContain('1 / 4');
    expect(row(fixture, 'w4').textContent).toContain('Offline');
  });

  it('offers Drain only on connected workers that are not draining', () => {
    const fixture = setup([IDLE, OFFLINE, SELF_DRAINED]);
    expect(button(row(fixture, 'Builder'), 'Drain')).toBeDefined();
    expect(button(row(fixture, 'w4'), 'Drain')).toBeUndefined();
    expect(row(fixture, 'Self drained').querySelector('button')).toBeNull();
  });

  it('explains a drain the worker started on its own', () => {
    const fixture = setup([SELF_DRAINED]);
    const badge = Array.from(row(fixture, 'Self drained').querySelectorAll('gr-badge')).find((b) => b.textContent?.includes('Draining'))!;
    badge.dispatchEvent(new Event('mouseenter'));
    expect(document.querySelector('.gr-tooltip-overlay')?.textContent).toContain('started this drain on its own');
  });

  it('asks first, then drains and marks the row', () => {
    const fixture = setup([IDLE]);
    const success = vi.spyOn(TestBed.inject(NotifyService), 'success');
    button(row(fixture, 'Builder'), 'Drain')!.click();
    fixture.detectChanges();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Worker Drain', target: 'Builder' }));
    expect(setWorkerDraining).toHaveBeenCalledWith('w1', true);
    expect(success).toHaveBeenCalledWith('Drain started');
    expect(button(row(fixture, 'Builder'), 'Undrain')).toBeDefined();
  });

  it('sends nothing when the drain is cancelled', () => {
    const fixture = setup([IDLE], false);
    button(row(fixture, 'Builder'), 'Drain')!.click();
    expect(setWorkerDraining).not.toHaveBeenCalled();
  });

  it('stops an admin drain at once and reports a failure', () => {
    const fixture = setup([ADMIN_DRAINED]);
    const refused = new Error('boom');
    setWorkerDraining.mockReturnValueOnce(throwError(() => refused));
    const failure = vi.spyOn(TestBed.inject(NotifyService), 'failure');
    button(row(fixture, 'Admin drained'), 'Undrain')!.click();
    expect(confirm).not.toHaveBeenCalled();
    expect(setWorkerDraining).toHaveBeenCalledWith('w2', false);
    expect(failure).toHaveBeenCalledWith('Failed to stop drain', refused);
  });

  it('points to the team and project pages when no worker is registered', () => {
    const fixture = setup([]);
    expect(fixture.nativeElement.textContent).toContain('No workers registered');
    expect(fixture.nativeElement.textContent).toContain('Register a worker on the team or project page.');
  });
});
