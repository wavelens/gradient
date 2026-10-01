/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { of } from 'rxjs';
import { IntegrationsComponent } from './integrations.component';
import { IntegrationsService } from '@core/services/integrations.service';
import { ProjectsService } from '@core/services/projects.service';
import { ProjectAccessService } from '@core/services/project-access.service';
import { AccessState, Integration } from '@core/models';

const baseIntegration: Integration = {
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
};

const githubOutbound: Integration = {
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
};

const githubInbound: Integration = {
  ...githubOutbound,
  id: 'g2',
  kind: 'inbound',
};

function activatedRouteStub() {
  return {
    snapshot: { paramMap: convertToParamMap({ project: 'acme' }) },
  } as Partial<ActivatedRoute>;
}

function setup(
  access: AccessState,
  integrations: Integration[],
  githubAppAvailable = false,
  githubAppInstallUrl: string | null = null,
): ComponentFixture<IntegrationsComponent> {
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

  it('shows but disables New Integration / Edit / Delete under state-managed access', async () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true }, [baseIntegration]);
    await settled(fixture);
    const newBtn = findByText(fixture.nativeElement, 'new integration') as HTMLButtonElement | null;
    expect(newBtn).not.toBeNull();
    expect(newBtn!.disabled).toBe(true);

    const editButtons = findAllByText(fixture.nativeElement, 'edit');
    const deleteButtons = findAllByText(fixture.nativeElement, 'delete');
    expect(editButtons.length).toBeGreaterThan(0);
    expect(deleteButtons.length).toBeGreaterThan(0);
    expect(editButtons.every((b) => b.disabled)).toBe(true);
    expect(deleteButtons.every((b) => b.disabled)).toBe(true);
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
