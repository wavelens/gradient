/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { EMPTY, Subject, of, throwError } from 'rxjs';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
import { IntegrationsComponent } from './integrations.component';
import { IntegrationsService } from '@core/services/integrations.service';
import { ProjectsService } from '@core/services/projects.service';
import { ProjectAccessService } from '@core/services/project-access.service';
import { AccessState, IntegrationResponse } from '@core/models';
import { ApiError } from '@core/services/api.service';
import { MessageService } from '@gradient/ui/ui';

const baseIntegration: IntegrationResponse = {
  id: 'i1',
  project: 'o',
  name: 'gitea-prod',
  display_name: 'Gitea Prod',
  kind: 'inbound',
  git_host_type: 'gitea',
  endpoint_url: null,
  has_secret: true,
  has_access_token: false,
  allowed_ips: [],
  created_by: 'u',
  created_at: '2026-01-01T00:00:00Z',
  managed: false,
};

const githubOutbound: IntegrationResponse = {
  id: 'g1',
  project: 'o',
  name: 'github-app',
  display_name: 'GitHub App',
  kind: 'outbound',
  git_host_type: 'github',
  endpoint_url: null,
  has_secret: false,
  has_access_token: false,
  allowed_ips: [],
  created_by: 'u',
  created_at: '2026-01-01T00:00:00Z',
  installation_id: 99999,
  account_login: 'acme-project',
  managed: false,
};

const githubInbound: IntegrationResponse = {
  ...githubOutbound,
  id: 'g2',
  kind: 'inbound',
};

function activatedRouteStub() {
  return {
    snapshot: { paramMap: convertToParamMap({ project: 'acme' }) },
  } as Partial<ActivatedRoute>;
}

const confirm = vi.fn();

function setup(
  access: AccessState,
  integrations: IntegrationResponse[],
  githubAppAvailable = false,
  githubAppInstallUrl: string | null = null,
): ComponentFixture<IntegrationsComponent> {
  confirm.mockReset().mockImplementation((request: ConfirmRequest) => request.action());
  TestBed.configureTestingModule({
    imports: [IntegrationsComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: activatedRouteStub() },
      {
        provide: IntegrationsService,
        useValue: {
          listProjectIntegrations: () => of(integrations),
        },
      },
      {
        provide: ProjectsService,
        useValue: {
          getProject: () =>
            of({
              id: 'o',
              display_name: 'Acme',
              github_app_available: githubAppAvailable,
              github_app_install_url: githubAppInstallUrl,
            }),
        },
      },
      { provide: ProjectAccessService, useValue: { forProject: () => Promise.resolve(access) } },
      { provide: ConfirmService, useValue: { confirm } },
    ],
  });
  return TestBed.createComponent(IntegrationsComponent);
}

async function settled(fixture: ComponentFixture<IntegrationsComponent>) {
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

describe('IntegrationsComponent - access gating', () => {
  it('hides New Integration / Edit / Delete under read-only access', async () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false }, [baseIntegration]);
    await settled(fixture);
    expect(findByText(fixture.nativeElement, 'new integration')).toBeNull();
    expect(findByText(fixture.nativeElement, 'edit')).toBeNull();
    expect(findByText(fixture.nativeElement, 'delete')).toBeNull();
  });

  it('disables New Integration in a state-managed project but keeps Edit / Delete of an unmanaged integration', async () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true }, [baseIntegration]);
    await settled(fixture);
    const newBtn = findByText(fixture.nativeElement, 'new integration') as HTMLButtonElement | null;
    expect(newBtn).not.toBeNull();
    expect(newBtn!.disabled).toBe(true);

    const editButtons = findAllByText(fixture.nativeElement, 'edit');
    const deleteButtons = findAllByText(fixture.nativeElement, 'delete');
    expect(editButtons.length).toBeGreaterThan(0);
    expect(deleteButtons.length).toBeGreaterThan(0);
    expect(editButtons.some((b) => b.disabled)).toBe(false);
    expect(deleteButtons.some((b) => b.disabled)).toBe(false);
  });

  it('shows but disables Edit / Delete of a state-managed integration', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [
      { ...baseIntegration, managed: true },
    ]);
    await settled(fixture);
    const editButtons = findAllByText(fixture.nativeElement, 'edit');
    const deleteButtons = findAllByText(fixture.nativeElement, 'delete');
    expect(editButtons.length).toBeGreaterThan(0);
    expect(deleteButtons.length).toBeGreaterThan(0);
    expect(editButtons.some((b) => !b.disabled)).toBe(false);
    expect(deleteButtons.some((b) => !b.disabled)).toBe(false);
  });

  it('shows working New Integration / Edit / Delete under full access', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [baseIntegration]);
    await settled(fixture);
    const newBtn = findByText(fixture.nativeElement, 'new integration') as HTMLButtonElement | null;
    expect(newBtn).not.toBeNull();
    expect(newBtn!.disabled).toBe(false);
    expect(findByText(fixture.nativeElement, 'edit')).not.toBeNull();
    expect(findByText(fixture.nativeElement, 'delete')).not.toBeNull();
  });
});

describe('IntegrationsComponent - github installations', () => {
  it('githubInstallations() returns only outbound github rows', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [
      baseIntegration,
      githubOutbound,
      githubInbound,
    ]);
    await settled(fixture);
    const comp = fixture.componentInstance;
    expect(comp.githubInstallations().length).toBe(1);
    expect(comp.githubInstallations()[0].id).toBe('g1');
  });

  it('githubAppInstalled() is false when no github outbound rows exist', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [baseIntegration]);
    await settled(fixture);
    expect(fixture.componentInstance.githubAppInstalled()).toBe(false);
  });

  it('githubAppInstalled() is true when a github outbound row exists', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [githubOutbound]);
    await settled(fixture);
    expect(fixture.componentInstance.githubAppInstalled()).toBe(true);
  });

  it('github rows show Delete but not Edit under full access', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [githubOutbound]);
    await settled(fixture);
    expect(findByText(fixture.nativeElement, 'delete')).not.toBeNull();
    expect(findByText(fixture.nativeElement, 'edit')).toBeNull();
  });
});

describe('IntegrationsComponent - required webhook events', () => {
  it('lists PR/comment/review events for Gitea/Forgejo and never push-only', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [baseIntegration]);
    await settled(fixture);
    const comp = fixture.componentInstance;
    const events = comp.requiredWebhookEvents(baseIntegration.id);
    expect(events).toContain('Issue Comment');
    expect(events).toContain('Pull Request Review');
  });

  it('lists Merge request + Comments (note) events for GitLab', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [baseIntegration]);
    await settled(fixture);
    const comp = fixture.componentInstance;
    comp.setInboundGitHost(baseIntegration.id, 'gitlab');
    const events = comp.requiredWebhookEvents(baseIntegration.id);
    expect(events).toContain('Merge request');
    expect(events).toContain('Comments (note)');
  });
});

describe('IntegrationsComponent - GitHub App install banner', () => {
  const installUrl = 'https://github.com/apps/gradient-ci/installations/new';
  const fullAccess = { managed: false, canEdit: true, canTrigger: true };

  it('links the App install page while no installation is linked', async () => {
    const fixture = setup(fullAccess, [], true, installUrl);
    await settled(fixture);
    const link = fixture.nativeElement.querySelector(
      '[data-testid="no-github-app-banner"] a',
    ) as HTMLAnchorElement | null;
    expect(link?.getAttribute('href')).toBe(installUrl);
  });

  it('hides once an installation is linked', async () => {
    const fixture = setup(fullAccess, [githubOutbound], true, installUrl);
    await settled(fixture);
    expect(fixture.nativeElement.querySelector('[data-testid="no-github-app-banner"]')).toBeNull();
  });
});

describe('IntegrationsComponent - create github integration', () => {
  it('createIntegration sends git_host_type=github with installation_id', () => {
    const createSpy = vi.fn().mockReturnValue(of({}));
    TestBed.configureTestingModule({
      imports: [IntegrationsComponent],
      providers: [
        provideRouter([]),
        provideHttpClient(),
        provideHttpClientTesting(),
        { provide: ActivatedRoute, useValue: activatedRouteStub() },
        {
          provide: IntegrationsService,
          useValue: {
            listProjectIntegrations: () => of([]),
            createProjectIntegration: createSpy,
          },
        },
        {
          provide: ProjectsService,
          useValue: {
            getProject: () => of({ id: 'o', display_name: 'Acme', github_app_available: true }),
          },
        },
        { provide: ProjectAccessService, useValue: { forProject: () => Promise.resolve({ managed: false, canEdit: true, canTrigger: true }) } },
      ],
    });
    const fixture = TestBed.createComponent(IntegrationsComponent);
    fixture.detectChanges();

    const comp = fixture.componentInstance;
    comp.formData.name = 'github-app';
    comp.formData.git_host_type = 'github';
    comp.formData.installation_id = '12345';
    comp.formData.kind = 'outbound';
    comp.createIntegration();

    expect(createSpy).toHaveBeenCalledOnce();
    expect(createSpy).toHaveBeenCalledWith('acme', expect.objectContaining({
      name: 'github-app',
      git_host_type: 'github',
      installation_id: 12345,
    }));
  });

  it('createIntegration rejects a non-integer installation_id', () => {
    const createSpy = vi.fn().mockReturnValue(of({}));
    TestBed.configureTestingModule({
      imports: [IntegrationsComponent],
      providers: [
        provideRouter([]),
        provideHttpClient(),
        provideHttpClientTesting(),
        { provide: ActivatedRoute, useValue: activatedRouteStub() },
        {
          provide: IntegrationsService,
          useValue: {
            listProjectIntegrations: () => of([]),
            createProjectIntegration: createSpy,
          },
        },
        {
          provide: ProjectsService,
          useValue: {
            getProject: () => of({ id: 'o', display_name: 'Acme', github_app_available: true }),
          },
        },
        { provide: ProjectAccessService, useValue: { forProject: () => Promise.resolve({ managed: false, canEdit: true, canTrigger: true }) } },
      ],
    });
    const fixture = TestBed.createComponent(IntegrationsComponent);
    fixture.detectChanges();

    const comp = fixture.componentInstance;
    comp.formData.name = 'github-app';
    comp.formData.git_host_type = 'github';
    comp.formData.installation_id = 'not-a-number';
    comp.createIntegration();

    expect(createSpy).not.toHaveBeenCalled();
    expect(comp.errorMessage()).toContain('positive integer');
  });
});

describe('IntegrationsComponent - load errors', () => {
  it('shows a failed load as an error, never as the empty state and loads again on Try Again', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, []);
    const service = TestBed.inject(IntegrationsService) as unknown as { listProjectIntegrations: () => unknown };
    service.listProjectIntegrations = () => throwError(() => new Error('database is down'));
    await settled(fixture);
    const root = fixture.nativeElement as HTMLElement;
    expect(root.textContent).toContain('Failed to load integrations');
    expect(root.textContent).toContain('database is down');
    expect(root.textContent).not.toContain('No integrations');

    service.listProjectIntegrations = () => of([]);
    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')!.click();
    await settled(fixture);
    expect(root.textContent).not.toContain('Failed to load integrations');
  });
});

describe('IntegrationsComponent - write failures', () => {
  const writable: AccessState = { managed: false, canEdit: true, canTrigger: true };

  it('asks before deleting an integration, naming it', async () => {
    const fixture = setup(writable, [baseIntegration]);
    const deleteProjectIntegration = vi.fn(() => of(undefined));
    Object.assign(TestBed.inject(IntegrationsService), { deleteProjectIntegration });
    await settled(fixture);
    findByText(fixture.nativeElement, 'delete')!.click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Delete integration', target: 'Gitea Prod' }));
    expect(deleteProjectIntegration).toHaveBeenCalledWith('acme', 'i1');
    expect(TestBed.inject(MessageService).messages()).toContainEqual(
      expect.objectContaining({ severity: 'success', summary: 'Integration deleted' }),
    );
  });

  it('sends no delete when the confirmation is cancelled', async () => {
    const fixture = setup(writable, [baseIntegration]);
    const deleteProjectIntegration = vi.fn(() => of(undefined));
    Object.assign(TestBed.inject(IntegrationsService), { deleteProjectIntegration });
    confirm.mockImplementation(() => EMPTY);
    await settled(fixture);
    findByText(fixture.nativeElement, 'delete')!.click();
    expect(deleteProjectIntegration).not.toHaveBeenCalled();
  });

  it.each([
    { action: 'webhook secret', run: (c: IntegrationsComponent) => c.clearSecret(), title: 'Clear webhook secret', patch: { secret: '' } },
    { action: 'access token', run: (c: IntegrationsComponent) => c.clearAccessToken(), title: 'Clear access token', patch: { access_token: '' } },
  ])('asks before clearing the stored $action and sends nothing on cancel', async ({ run, title, patch }) => {
    const fixture = setup(writable, [baseIntegration]);
    const patchProjectIntegration = vi.fn(() => of(undefined));
    Object.assign(TestBed.inject(IntegrationsService), { patchProjectIntegration });
    await settled(fixture);
    const component = fixture.componentInstance;
    component.editingIntegration.set(baseIntegration);
    confirm.mockImplementationOnce(() => EMPTY);
    run(component);
    expect(patchProjectIntegration).not.toHaveBeenCalled();
    run(component);
    expect(confirm).toHaveBeenLastCalledWith(expect.objectContaining({ title, target: 'Gitea Prod' }));
    expect(patchProjectIntegration).toHaveBeenCalledWith('acme', 'i1', patch);
    expect(component.editingIntegration()).toBeNull();
  });

  it('keeps a failed create inside the open New Integration dialog', async () => {
    const fixture = setup(writable, []);
    const service = TestBed.inject(IntegrationsService) as unknown as { createProjectIntegration: () => unknown };
    service.createProjectIntegration = () => throwError(() => new ApiError('Name gitea-prod is already taken', 409));
    await settled(fixture);
    const comp = fixture.componentInstance;
    comp.openCreateDialog();
    comp.formData.name = 'gitea-prod';
    comp.createIntegration();
    fixture.detectChanges();
    expect(document.querySelector('.gr-dialog gr-message-banner')?.textContent).toContain('Name gitea-prod is already taken');
    expect(comp.showCreateDialog()).toBe(true);
  });

  it('keeps the New Integration dialog open and sends one create while it runs', async () => {
    const fixture = setup(writable, []);
    const create = vi.fn(() => new Subject<IntegrationResponse>());
    (TestBed.inject(IntegrationsService) as unknown as { createProjectIntegration: typeof create }).createProjectIntegration = create;
    await settled(fixture);
    const comp = fixture.componentInstance;
    comp.openCreateDialog();
    comp.formData.name = 'gitea-prod';
    fixture.detectChanges();
    const dialog = document.querySelector('.gr-dialog')!;
    (findByText(dialog as HTMLElement, 'create') as HTMLButtonElement).click();
    fixture.detectChanges();
    (findByText(dialog as HTMLElement, 'create') as HTMLButtonElement).click();
    dialog.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', keyCode: 27, bubbles: true }));
    fixture.detectChanges();
    expect(create).toHaveBeenCalledTimes(1);
    expect(comp.showCreateDialog()).toBe(true);
    expect(dialog.getAttribute('aria-busy')).toBe('true');
  });
});
