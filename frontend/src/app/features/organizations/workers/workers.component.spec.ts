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
  enable_fetch: true,
  enable_eval: true,
  enable_build: true,
};

const workerManaged: Worker = {
  worker_id: 'w2',
  display_name: 'Nix-managed',
  managed: true,
  active: true,
  enable_fetch: true,
  enable_eval: true,
  enable_build: true,
};

function setup(opts: {
  access: AccessState;
  workers: Worker[];
  caches: { id: string; name: string }[];
}) {
  const workersService = { getWorkers: vi.fn(() => of(opts.workers)) };
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

describe('WorkersComponent — no-cache banner (existing)', () => {
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

describe('WorkersComponent — access gating', () => {
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
