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
import { ProjectsService } from '@core/services/projects.service';
import { ProjectAccessService } from '@core/services/project-access.service';
import { ConfigService } from '@core/services/config.service';
import { AccessState } from '@core/models/access.model';
import { Worker } from '@core/models/worker.model';
import { TeamsService } from '@core/services/teams.service';
import { AuthService } from '@core/services/auth.service';
import { signal } from '@angular/core';

type MockedProjects = {
  getProject: ReturnType<typeof vi.fn>;
  getSubscribedCaches: ReturnType<typeof vi.fn>;
};

function activatedRouteStub() {
  return { snapshot: { paramMap: convertToParamMap({ project: 'demo' }) } } as Partial<ActivatedRoute>;
}

const workerUnmanaged: Worker = {
  worker_id: 'w1',
  display_name: 'Builder',
  managed: false,
  active: true,
  gradient_ci: false,
  connected: false,
  enable_fetch: true,
  enable_eval: true,
  enable_build: true,
};

const workerManaged: Worker = {
  worker_id: 'w2',
  display_name: 'Nix-managed',
  managed: true,
  active: true,
  gradient_ci: false,
  connected: false,
  enable_fetch: true,
  enable_eval: true,
  enable_build: true,
};

const workerOfTeam: Worker = {
  worker_id: 'w3',
  display_name: 'Team worker',
  managed: true,
  active: true,
  team: 'platform',
  gradient_ci: false,
  connected: false,
  enable_fetch: true,
  enable_eval: true,
  enable_build: true,
};

function setup(opts: {
  access: AccessState;
  workers: Worker[];
  caches: { id: string; name: string }[];
  testWorker?: ReturnType<typeof vi.fn>;
  gradientCi?: boolean;
  memberOf?: string[];
}) {
  const workersService = {
    getWorkers: vi.fn(() => of(opts.workers)),
    testWorker: opts.testWorker ?? vi.fn(() => of({ ok: true, connected: true, authorized_for_project: true, message: 'ok' })),
  };
  const projects: MockedProjects = {
    getProject: vi.fn(() => of({ id: 'project-uuid', display_name: 'Project' } as never)),
    getSubscribedCaches: vi.fn(() => of(opts.caches)),
  };
  TestBed.configureTestingModule({
    imports: [WorkersComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: WorkersService, useValue: workersService },
      { provide: ProjectsService, useValue: projects },
      {
        provide: TeamsService,
        useValue: {
          list: () => of((opts.memberOf ?? ['platform']).map((name) => ({ name, display_name: name }))),
          projectGrants: () => of([]),
        },
      },
      { provide: AuthService, useValue: { user: signal({ superuser: false }) } },
      { provide: ProjectAccessService, useValue: { forProject: () => Promise.resolve(opts.access) } },
      { provide: ActivatedRoute, useValue: activatedRouteStub() },
      {
        provide: ConfigService,
        useValue: { gradientCiEnabled: opts.gradientCi ?? true, gradientCiUrl: 'https://servers.gradient.ci' },
      },
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
  it('shows the banner when the project has no subscribed caches', async () => {
    const fixture = setup({ access: { managed: false, canEdit: true, canTrigger: true }, workers: [], caches: [] });
    await settled(fixture);
    const banner = fixture.nativeElement.querySelector('[data-testid="no-cache-banner"]');
    expect(banner, 'banner element').toBeTruthy();
  });

  it('hides the banner when the project has at least one subscribed cache', async () => {
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
  it('hides Register Worker button under read-only project access', async () => {
    const fixture = setup({ access: { managed: false, canEdit: false, canTrigger: false }, workers: [workerUnmanaged], caches: [{ id: 'c', name: 'c' }] });
    await settled(fixture);
    expect(findByText(fixture.nativeElement, 'register worker')).toBeNull();
  });

  it('hides per-row Edit / Activate / Delete under read-only project access', async () => {
    const fixture = setup({ access: { managed: false, canEdit: false, canTrigger: false }, workers: [workerUnmanaged], caches: [{ id: 'c', name: 'c' }] });
    await settled(fixture);
    expect(findByText(fixture.nativeElement, 'edit')).toBeNull();
    expect(findByText(fixture.nativeElement, 'delete')).toBeNull();
    expect(findByText(fixture.nativeElement, 'deactivate')).toBeNull();
  });

  it('shows but disables Register Worker under state-managed project', async () => {
    const fixture = setup({ access: { managed: true, canEdit: true, canTrigger: true }, workers: [], caches: [{ id: 'c', name: 'c' }] });
    await settled(fixture);
    const btn = findByText(fixture.nativeElement, 'register worker') as HTMLButtonElement | null;
    expect(btn).not.toBeNull();
    expect(btn!.disabled).toBe(true);
  });

  it('disables a managed worker row even in an unmanaged, writable project', async () => {
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

describe('WorkersComponent - team workers', () => {
  it('renders a team worker read-only with a link to its team', async () => {
    const fixture = setup({
      access: { managed: false, canEdit: true, canTrigger: true },
      workers: [workerOfTeam],
      caches: [{ id: 'c', name: 'c' }],
    });
    await settled(fixture);

    const badge = (Array.from(fixture.nativeElement.querySelectorAll('gr-badge')) as HTMLElement[])
      .find((el) => (el.textContent ?? '').trim() === 'Team platform');
    expect(badge, 'Team badge').toBeTruthy();
    const link = (fixture.nativeElement as HTMLElement).querySelector('a[href="/team/platform/workers"]');
    expect(link?.textContent).toContain('Manage on team');
    expect(findByText(fixture.nativeElement, 'edit')).toBeNull();
    expect(findByText(fixture.nativeElement, 'deactivate')).toBeNull();
    expect(findByText(fixture.nativeElement, 'delete')).toBeNull();
  });

  it('offers no team link to a viewer outside the team, whose team page would not load', async () => {
    const fixture = setup({
      access: { managed: false, canEdit: true, canTrigger: true },
      workers: [workerOfTeam],
      caches: [{ id: 'c', name: 'c' }],
      memberOf: [],
    });
    await settled(fixture);

    expect((fixture.nativeElement as HTMLElement).querySelector('a[href="/team/platform/workers"]')).toBeNull();
  });

  it('keeps Deactivate usable on a managed worker in a managed project, since state restores it on restart', async () => {
    const fixture = setup({
      access: { managed: true, canEdit: true, canTrigger: true },
      workers: [workerManaged],
      caches: [{ id: 'c', name: 'c' }],
    });
    await settled(fixture);
    const deactivate = findByText(fixture.nativeElement, 'deactivate') as HTMLButtonElement | null;
    const edit = findByText(fixture.nativeElement, 'edit') as HTMLButtonElement | null;
    expect(deactivate!.disabled).toBe(false);
    expect(edit!.disabled).toBe(true);
  });

  it('fireTest calls the service and surfaces the result via a toast', async () => {
    const testWorker = vi.fn(() => of({ ok: true, connected: true, authorized_for_project: true, message: 'reachable' }));
    const fixture = setup({
      access: { managed: false, canEdit: true, canTrigger: true },
      workers: [workerOfTeam],
      caches: [{ id: 'c', name: 'c' }],
      testWorker,
    });
    await settled(fixture);
    const cmp = fixture.componentInstance;
    const addSpy = vi.spyOn(cmp['messageService'], 'add');

    cmp.fireTest(workerOfTeam);

    expect(testWorker).toHaveBeenCalledWith('demo', workerOfTeam.worker_id);
    expect(cmp.testingId()).toBeNull();
    expect(addSpy).toHaveBeenCalledWith(
      expect.objectContaining({ severity: 'success', detail: 'reachable' }),
    );
  });
});

describe('WorkersComponent - connection badge', () => {
  const live = {
    capabilities: { fetch: true, eval: true, build: true, federate: false },
    architectures: [], system_features: [], max_concurrent_builds: 1, assigned_job_count: 0, draining: false,
  } as unknown as Worker['live'];
  const badges = (fixture: ComponentFixture<WorkersComponent>) =>
    Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('gr-badge')).map((b) => b.textContent?.trim());

  it('marks a deactivated worker that is still connected as on its way out', async () => {
    const fixture = setup({
      access: { managed: false, canEdit: true, canTrigger: true },
      workers: [{ ...workerUnmanaged, active: false, live }],
      caches: [{ id: 'c', name: 'cache-1' }],
    });
    await settled(fixture);
    expect(badges(fixture)).toContain('Disconnecting');
    expect(badges(fixture)).not.toContain('Connected');
  });

  it('shows an active connected worker as connected', async () => {
    const fixture = setup({
      access: { managed: false, canEdit: true, canTrigger: true },
      workers: [{ ...workerUnmanaged, live }],
      caches: [{ id: 'c', name: 'cache-1' }],
    });
    await settled(fixture);
    expect(badges(fixture)).toContain('Connected');
  });
});

function buttonsLabelled(root: Element, label: string): HTMLButtonElement[] {
  return (Array.from(root.querySelectorAll('button')) as HTMLButtonElement[]).filter(
    (b) => b.querySelector('.gr-button__label')?.textContent?.trim() === label,
  );
}

function entry(fixture: ComponentFixture<WorkersComponent>): HTMLElement | null {
  return (fixture.nativeElement as HTMLElement).querySelector('[data-testid="gradient-ci-entry"]');
}

const gciRegistration: Worker = {
  worker_id: 'g1',
  display_name: 'Gradient.CI Servers',
  managed: false,
  active: true,
  gradient_ci: true,
  connected: false,
  last_error: {
    reason: '401 unknown worker id or wrong token',
    at: '2026-10-02T12:00:00',
    direction: 'outbound',
    before_auth: false,
  },
  enable_fetch: false,
  enable_eval: true,
  enable_build: true,
};

const gciTeam: Worker = { ...gciRegistration, worker_id: 'g2', team: 'platform', last_error: undefined };
const writable: AccessState = { managed: false, canEdit: true, canTrigger: true };
const caches = [{ id: 'c', name: 'c' }];

describe('WorkersComponent - Gradient.CI Servers entry', () => {
  it('offers Connect when nothing is connected', async () => {
    const fixture = setup({ access: writable, workers: [], caches });
    await settled(fixture);
    expect(buttonsLabelled(entry(fixture)!, 'Connect').length).toBe(1);
  });

  it('shows a project connection with its offline reason and Disconnect', async () => {
    const fixture = setup({ access: writable, workers: [gciRegistration], caches });
    await settled(fixture);
    expect(entry(fixture)!.textContent).toContain('401 unknown worker id or wrong token');
    expect(buttonsLabelled(entry(fixture)!, 'Disconnect').length).toBe(1);
  });

  it('shows a connection of a granted team as coming from that team', async () => {
    const fixture = setup({ access: writable, workers: [gciTeam], caches });
    await settled(fixture);
    expect(entry(fixture)!.textContent).toContain('Via team platform');
    expect(buttonsLabelled(entry(fixture)!, 'Disconnect').length).toBe(0);
  });

  it('hides the entry when the option is off and nothing is connected', async () => {
    const fixture = setup({ access: writable, workers: [], caches, gradientCi: false });
    await settled(fixture);
    expect(entry(fixture)).toBeNull();
  });

  it('keeps an existing connection listed when the option is off', async () => {
    const fixture = setup({ access: writable, workers: [gciRegistration], caches, gradientCi: false });
    await settled(fixture);
    expect(buttonsLabelled(entry(fixture)!, 'Disconnect').length).toBe(1);
    expect(buttonsLabelled(entry(fixture)!, 'Connect').length).toBe(0);
  });

  it('shows the state but no Connect to read-only members', async () => {
    const fixture = setup({ access: { managed: false, canEdit: false, canTrigger: false }, workers: [], caches });
    await settled(fixture);
    expect(entry(fixture)).not.toBeNull();
    expect(buttonsLabelled(entry(fixture)!, 'Connect').length).toBe(0);
  });

  it('shows the last failure under an offline registered worker', async () => {
    const offline: Worker = {
      ...workerUnmanaged,
      last_error: {
        reason: 'dial timed out after 10 s',
        at: '2026-10-02T12:00:00',
        direction: 'outbound',
        before_auth: false,
      },
    };
    const fixture = setup({ access: writable, workers: [offline], caches });
    await settled(fixture);
    expect((fixture.nativeElement as HTMLElement).textContent).toContain('dial timed out after 10 s');
  });
});
