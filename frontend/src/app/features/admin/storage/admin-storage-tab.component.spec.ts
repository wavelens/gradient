/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { EMPTY, type Observable, of, throwError } from 'rxjs';
import { AdminStorageTabComponent, getCacheStorageLimitLabel } from './admin-storage-tab.component';
import { AdminService } from '@core/services/admin.service';
import { NotifyService } from '@core/services/notify.service';
import { ConfirmService, type ConfirmRequest } from '@shared/confirm';
import { AdminStorageResponse } from '@core/models';

const PROJECT = { id: 'p1', name: 'web', display_name: 'Web' };
const STORAGE: AdminStorageResponse = {
  disk_total_bytes: 4 * 1024 ** 3,
  disk_free_bytes: 1024 ** 3,
  caches: [{ id: 'c1', name: 'main', display_name: 'Main', max_storage_gb: 0, packages: 7, bytes: 2048 }],
  projects: [{ project: PROJECT, packages: 3, bytes: 1024 }],
  tasks: [{
    id: 't1', name: 'build', display_name: 'Build', project: PROJECT, keep_evaluations: 10, evaluations: 4,
    oldest_evaluation_at: null, packages: 3, bytes: 1024,
  }],
  last_deep_gc: null,
};

let confirm: ReturnType<typeof vi.fn>;
let startDeepGc: ReturnType<typeof vi.fn>;
let startGcRound: ReturnType<typeof vi.fn>;

function setup(storage: AdminStorageResponse = STORAGE, confirmed = true): ComponentFixture<AdminStorageTabComponent> {
  confirm = vi.fn((request: ConfirmRequest): Observable<unknown> => (confirmed ? request.action() : EMPTY));
  startDeepGc = vi.fn(() => of({ task_id: 't2', status: 'pending' }));
  startGcRound = vi.fn(() => of(undefined));
  TestBed.configureTestingModule({
    imports: [AdminStorageTabComponent],
    providers: [
      provideRouter([]),
      { provide: ConfirmService, useValue: { confirm } },
      { provide: AdminService, useValue: { getDiskAndStorageUsageByCacheProjectAndTask: () => of(storage), startDeepGc, startGcRound } },
    ],
  });
  const fixture = TestBed.createComponent(AdminStorageTabComponent);
  fixture.detectChanges();
  return fixture;
}

function button(fixture: ComponentFixture<AdminStorageTabComponent>, label: string): HTMLButtonElement {
  return Array.from((fixture.nativeElement as HTMLElement).querySelectorAll<HTMLButtonElement>('.actions button'))
    .find((b) => b.textContent?.includes(label))!;
}

describe('AdminStorageTabComponent', () => {
  afterEach(() => TestBed.resetTestingModule());

  it('labels a zero cache limit as unlimited', () => {
    expect(getCacheStorageLimitLabel(0)).toBe('Unlimited');
    expect(getCacheStorageLimitLabel(50)).toBe('50 GB');
  });

  it('shows free disk space and links caches and tasks to their settings', () => {
    const root = setup().nativeElement as HTMLElement;
    expect(root.querySelector('.tile')?.textContent).toContain('1.0 GiB of 4.0 GiB');
    expect(root.querySelector('gr-table.caches a')?.getAttribute('href')).toBe('/caches/main/settings');
    expect(root.querySelector('gr-table.caches')?.textContent).toContain('Unlimited');
    expect(root.querySelector('gr-table.tasks a')?.getAttribute('href')).toBe('/project/web/task/build/settings');
  });

  it('says when no deep GC finished yet', () => {
    expect(setup().nativeElement.querySelector('.last-deep-gc').textContent).toContain('Deep GC is not complete.');
  });

  it('sums up what the last deep GC removed', () => {
    const last_deep_gc = { nars_scanned: 10, orphan_nars_removed: 2, blobs_scanned: 5, orphan_blobs_removed: 1, logs_scanned: 3, orphan_logs_removed: 0 };
    const text = setup({ ...STORAGE, last_deep_gc }).nativeElement.querySelector('.last-deep-gc').textContent;
    expect(text).toContain('Last deep GC: removed 2 of 10 NARs, 1 of 5 blobs, 0 of 3 logs.');
  });

  it('starts a GC round without asking', () => {
    const fixture = setup();
    const success = vi.spyOn(TestBed.inject(NotifyService), 'success');
    button(fixture, 'Run GC Now').click();
    expect(confirm).not.toHaveBeenCalled();
    expect(startGcRound).toHaveBeenCalledOnce();
    expect(success).toHaveBeenCalledWith('GC round started');
  });

  it('reports a GC round that failed to start', () => {
    const fixture = setup();
    const refused = new Error('boom');
    startGcRound.mockReturnValueOnce(throwError(() => refused));
    const failure = vi.spyOn(TestBed.inject(NotifyService), 'failure');
    button(fixture, 'Run GC Now').click();
    expect(failure).toHaveBeenCalledWith('Failed to start GC round', refused);
  });

  it('asks before a deep GC and sends nothing on cancel', () => {
    const fixture = setup(STORAGE, false);
    button(fixture, 'Run Deep GC').click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Run Deep GC' }));
    expect(startDeepGc).not.toHaveBeenCalled();
  });
});
