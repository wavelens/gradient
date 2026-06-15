/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { of } from 'rxjs';
import { ActivatedRoute, convertToParamMap } from '@angular/router';
import { WorkersComponent } from './workers.component';
import { WorkersService } from '@core/services/workers.service';
import { OrganizationsService } from '@core/services/organizations.service';
import { OrgAccessService } from '@core/services/org-access.service';
import { AccessState } from '@core/models/access.model';
import { Worker } from '@core/models/worker.model';

type MockedOrgs = {
  getOrganization: ReturnType<typeof vi.fn>;
  getSubscribedCaches: ReturnType<typeof vi.fn>;
};

function activatedRouteStub() {
  return { snapshot: { paramMap: convertToParamMap({ org: 'demo' }) } } as Partial<ActivatedRoute>;
}

const workerUnmanaged: Worker = {
  worker_id: 'w1',
  display_name: 'Builder',
  managed: false,
  active: true,
  is_base: false,
  enable_fetch: true,
  enable_eval: true,
  enable_build: true,
};

const workerManaged: Worker = {
  worker_id: 'w2',
  display_name: 'Nix-managed',
  managed: true,
  active: true,
  is_base: false,
  enable_fetch: true,
  enable_eval: true,
  enable_build: true,
};

const workerBase: Worker = {
  worker_id: 'w3',
  display_name: 'Base worker',
  managed: true,
  active: true,
  is_base: true,
  enable_fetch: true,
  enable_eval: true,
  enable_build: true,
};

function setup(opts: {
  access: AccessState;
  workers: Worker[];
  caches: { id: string; name: string }[];
  testWorker?: ReturnType<typeof vi.fn>;
}) {
  const workersService = {
    getWorkers: vi.fn(() => of(opts.workers)),
    testWorker: opts.testWorker ?? vi.fn(() => of({ ok: true, connected: true, authorized_for_org: true, message: 'ok' })),
  };
  const orgs: MockedOrgs = {
    getOrganization: vi.fn(() => of({ id: 'org-uuid', display_name: 'Org' } as never)),
    getSubscribedCaches: vi.fn(() => of(opts.caches)),
  };
  TestBed.configureTestingModule({
    imports: [WorkersComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: WorkersService, useValue: workersService },
      { provide: OrganizationsService, useValue: orgs },
      { provide: OrgAccessService, useValue: { forOrg: () => Promise.resolve(opts.access) } },
      { provide: ActivatedRoute, useValue: activatedRouteStub() },
    ],
  });
  return TestBed.createComponent(WorkersComponent);
}

async function settled(fixture: ComponentFixture<WorkersComponent>) {
  fixture.detectChanges();
  await fixture.whenStable();
  fixture.detectChanges();
}

function findByText(root: HTMLElement, text: string): HTMLElement | null {
  const target = text.toLowerCase();
  return (Array.from(root.querySelectorAll('button')) as HTMLElement[]).find(
    (el) => (el.textContent ?? '').trim().toLowerCase().includes(target),
  ) ?? null;
}

function findAllByText(root: HTMLElement, text: string): HTMLButtonElement[] {
  const target = text.toLowerCase();
  return (Array.from(root.querySelectorAll('button')) as HTMLButtonElement[]).filter(
    (el) => (el.textContent ?? '').trim().toLowerCase().includes(target),
  );
}

describe('WorkersComponent - no-cache banner (existing)', () => {
  it('shows the banner when the org has no subscribed caches', async () => {
    const fixture = setup({ access: { managed: false, canEdit: true, canTrigger: true }, workers: [], caches: [] });
    await settled(fixture);
    const banner = fixture.nativeElement.querySelector('[data-testid="no-cache-banner"]');
    expect(banner, 'banner element').toBeTruthy();
  });

  it('hides the banner when the org has at least one subscribed cache', async () => {
    const fixture = setup({
      access: { managed: false, canEdit: true, canTrigger: true },
      workers: [],
      caches: [{ id: 'c', name: 'cache-1' }],
    });
    await settled(fixture);
    const banner = fixture.nativeElement.querySelector('[data-testid="no-cache-banner"]');
    expect(banner, 'banner element').toBeNull();
  });
});

describe('WorkersComponent - access gating', () => {
  it('hides Register Worker button under read-only org access', async () => {
    const fixture = setup({ access: { managed: false, canEdit: false, canTrigger: false }, workers: [workerUnmanaged], caches: [{ id: 'c', name: 'c' }] });
    await settled(fixture);
    expect(findByText(fixture.nativeElement, 'register worker')).toBeNull();
  });

  it('hides per-row Edit / Activate / Delete under read-only org access', async () => {
    const fixture = setup({ access: { managed: false, canEdit: false, canTrigger: false }, workers: [workerUnmanaged], caches: [{ id: 'c', name: 'c' }] });
    await settled(fixture);
    expect(findByText(fixture.nativeElement, 'edit')).toBeNull();
    expect(findByText(fixture.nativeElement, 'delete')).toBeNull();
    expect(findByText(fixture.nativeElement, 'deactivate')).toBeNull();
  });

  it('shows but disables Register Worker under state-managed org', async () => {
    const fixture = setup({ access: { managed: true, canEdit: true, canTrigger: true }, workers: [], caches: [{ id: 'c', name: 'c' }] });
    await settled(fixture);
    const btn = findByText(fixture.nativeElement, 'register worker') as HTMLButtonElement | null;
    expect(btn).not.toBeNull();
    expect(btn!.disabled).toBe(true);
  });

  it('disables a managed worker row even in an unmanaged, writable org', async () => {
    const fixture = setup({
      access: { managed: false, canEdit: true, canTrigger: true },
      workers: [workerUnmanaged, workerManaged],
      caches: [{ id: 'c', name: 'c' }],
    });
    await settled(fixture);
    const editButtons = findAllByText(fixture.nativeElement, 'edit');
    expect(editButtons.length).toBe(2);
    // Buttons appear in DOM order matching workers[] order
    expect(editButtons[0].disabled).toBe(false);
    expect(editButtons[1].disabled).toBe(true);
  });
});

describe('WorkersComponent - base workers', () => {
  it('renders a Base badge and leaves Activate/Fire Test enabled but Edit/Delete disabled', async () => {
    const fixture = setup({
      access: { managed: false, canEdit: true, canTrigger: true },
      workers: [workerBase],
      caches: [{ id: 'c', name: 'c' }],
    });
    await settled(fixture);

    const badge = (Array.from(fixture.nativeElement.querySelectorAll('.badge-base')) as HTMLElement[])
      .find((el) => (el.textContent ?? '').trim() === 'Base');
    expect(badge, 'Base badge').toBeTruthy();

    const deactivate = findByText(fixture.nativeElement, 'deactivate') as HTMLButtonElement | null;
    const fireTest = findByText(fixture.nativeElement, 'fire test') as HTMLButtonElement | null;
    const edit = findByText(fixture.nativeElement, 'edit') as HTMLButtonElement | null;
    const del = findByText(fixture.nativeElement, 'delete') as HTMLButtonElement | null;

    expect(deactivate!.disabled).toBe(false);
    expect(fireTest!.disabled).toBe(false);
    expect(edit!.disabled).toBe(true);
    expect(del!.disabled).toBe(true);
  });

  it('actionAccess drops the managed flag for base workers but not for normal managed workers', async () => {
    const fixture = setup({
      access: { managed: false, canEdit: true, canTrigger: true },
      workers: [],
      caches: [],
    });
    await settled(fixture);
    const cmp = fixture.componentInstance;

    expect(cmp.actionAccess(workerBase).managed).toBe(false);
    expect(cmp.actionAccess(workerBase).canEdit).toBe(true);
    expect(cmp.actionAccess(workerManaged).managed).toBe(true);
    expect(cmp.actionAccess(workerUnmanaged)).toEqual(cmp.rowAccess(workerUnmanaged));
  });

  it('fireTest calls the service and surfaces the result via a toast', async () => {
    const testWorker = vi.fn(() => of({ ok: true, connected: true, authorized_for_org: true, message: 'reachable' }));
    const fixture = setup({
      access: { managed: false, canEdit: true, canTrigger: true },
      workers: [workerBase],
      caches: [{ id: 'c', name: 'c' }],
      testWorker,
    });
    await settled(fixture);
    const cmp = fixture.componentInstance;
    const addSpy = vi.spyOn(cmp['messageService'], 'add');

    cmp.fireTest(workerBase);

    expect(testWorker).toHaveBeenCalledWith('demo', workerBase.worker_id);
    expect(cmp.testingId()).toBeNull();
    expect(addSpy).toHaveBeenCalledWith(
      expect.objectContaining({ severity: 'success', detail: 'reachable' }),
    );
  });
});
