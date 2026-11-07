/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { EMPTY, Subject, of, throwError } from 'rxjs';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
import { IntegrationsComponent } from './integrations.component';
import { IntegrationsService } from '@core/services/integrations.service';
import { TasksService } from '@core/services/tasks.service';
import { projectRouteStub } from '@core/resolvers/access-route.testing';
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
  connected_tasks: [],
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
  connected_tasks: [],
};

const githubInbound: IntegrationResponse = {
  ...githubOutbound,
  id: 'g2',
  kind: 'inbound',
};


const confirm = vi.fn();

function setup(
  access: AccessState,
  integrations: IntegrationResponse[],
  githubAppAvailable = false,
  githubAppInstallUrl: string | null = null,
  query: Record<string, string> = {},
): ComponentFixture<IntegrationsComponent> {
  confirm.mockReset().mockImplementation((request: ConfirmRequest) => request.action());
  TestBed.configureTestingModule({
    imports: [IntegrationsComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      {
        provide: ActivatedRoute,
        useFactory: () =>
          projectRouteStub(
            'acme',
            access,
            { github_app_available: githubAppAvailable, github_app_install_url: githubAppInstallUrl },
            query,
          ),
      },
      {
        provide: IntegrationsService,
        useValue: {
          listProjectIntegrations: () => of(integrations),
          listIntegrationDeliveries: () => of([]),
        },
      },
      { provide: ConfirmService, useValue: { confirm } },
      { provide: TasksService, useValue: { getTasks: () => of({ items: [] }) } },
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

  it('explains the missing permission instead of listing integrations', async () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false }, [baseIntegration]);
    await settled(fixture);
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('No access to Integrations');
    expect(text).toContain('manage integrations permission');
    expect(text).not.toContain(baseIntegration.name);
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
    const events = comp.requiredWebhookEvents(baseIntegration);
    expect(events).toContain('Issue Comment');
    expect(events).toContain('Pull Request Review');
  });

  it('lists Merge request + Comments (note) events for GitLab', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [baseIntegration]);
    await settled(fixture);
    const comp = fixture.componentInstance;
    const events = comp.requiredWebhookEvents({ ...baseIntegration, git_host_type: 'gitlab' });
    expect(events).toContain('Merge request');
    expect(events).toContain('Comments (note)');
  });
});

describe('IntegrationsComponent - webhook URL', () => {
  it('shows the webhook URL from the server, not from the browser address', async () => {
    const url = 'https://ci.example.com/api/v1/hooks/gitea/acme/gitea-prod';
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [{ ...baseIntegration, webhook_url: url }]);
    await settled(fixture);
    const inputs = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll<HTMLInputElement>('gr-copy-field input'));
    expect(inputs.map((input) => input.value)).toContain(url);
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

  it('asks before clearing the stored access token and sends nothing on cancel', async () => {
    const run = (c: IntegrationsComponent) => c.clearAccessToken();
    const title = 'Clear access token';
    const patch = { access_token: '' };
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

  it('leaves GitHub out of manual integrations, since the GitHub App connects them', async () => {
    const fixture = setup(writable, [], true);
    await settled(fixture);
    expect(fixture.componentInstance.selfHostedGitHostOptions.map((o) => o.value)).not.toContain('github');
  });

  it('warns that renaming a webhook receiver changes its webhook URL', async () => {
    const fixture = setup(writable, [baseIntegration]);
    await settled(fixture);
    const comp = fixture.componentInstance;
    comp.openEditDialog(baseIntegration);
    fixture.detectChanges();
    expect(document.querySelector('[data-testid="rename-breaks-webhook"]')).toBeNull();
    const name = document.querySelector<HTMLInputElement>('#edit-name')!;
    name.value = 'gitea-main';
    name.dispatchEvent(new Event('input'));
    fixture.detectChanges();
    expect(document.querySelector('[data-testid="rename-breaks-webhook"]')?.textContent).toContain('Webhook URL');
  });

  it('names the tasks a new integration connected to', async () => {
    const fixture = setup(writable, []);
    const create = vi.fn(() => of({ ...baseIntegration, connected_tasks: ['app', 'lib'] }));
    (TestBed.inject(IntegrationsService) as unknown as { createProjectIntegration: typeof create }).createProjectIntegration = create;
    await settled(fixture);
    const comp = fixture.componentInstance;
    comp.openCreateDialog();
    comp.formData.name = 'gitea-prod';
    comp.createIntegration();
    expect(TestBed.inject(MessageService).messages()).toContainEqual(
      expect.objectContaining({ severity: 'success', summary: 'Created integration. Connected to app, lib.' }),
    );
  });

  it('shows the generated webhook secret once after creating an inbound integration', async () => {
    const fixture = setup(writable, []);
    const create = vi.fn(() => of({ ...baseIntegration, generated_secret: 'f00d' }));
    (TestBed.inject(IntegrationsService) as unknown as { createProjectIntegration: typeof create }).createProjectIntegration = create;
    await settled(fixture);
    const comp = fixture.componentInstance;
    comp.openCreateDialog();
    comp.formData.name = 'gitea-prod';
    comp.createIntegration();
    fixture.detectChanges();
    expect(create.mock.calls[0]).not.toHaveProperty('1.secret');
    expect(comp.showSecretDialog()).toBe(true);
    expect(comp.createdInbound()?.secret).toBe('f00d');
  });

  it('opens New Integration for a trigger and offers the way back once the secret dialog closes', async () => {
    const fixture = setup(writable, [], false, null, { task: 'demo', trigger: 't9' });
    const create = vi.fn(() => of({ ...baseIntegration, id: 'new-id', generated_secret: 'f00d' }));
    (TestBed.inject(IntegrationsService) as unknown as { createProjectIntegration: typeof create }).createProjectIntegration = create;
    await settled(fixture);
    const comp = fixture.componentInstance;
    expect(comp.showCreateDialog()).toBe(true);
    comp.formData.name = 'gitea-prod';
    comp.createIntegration();
    fixture.detectChanges();
    const banner = () => (fixture.nativeElement as HTMLElement).querySelector('[data-testid="back-to-trigger-banner"] a');
    expect(banner()).toBeNull();

    comp.showSecretDialog.set(false);
    fixture.detectChanges();
    expect(banner()?.getAttribute('href')).toBe('/project/acme/task/demo/triggers?edit=t9&integration=new-id');
  });

  it('offers no way back to a trigger after creating an outbound integration', async () => {
    const fixture = setup(writable, [], false, null, { task: 'demo', trigger: 't9' });
    const create = vi.fn(() => of({ ...githubOutbound, id: 'new-id' }));
    (TestBed.inject(IntegrationsService) as unknown as { createProjectIntegration: typeof create }).createProjectIntegration = create;
    await settled(fixture);
    const comp = fixture.componentInstance;
    comp.formData = { ...comp.formData, name: 'reporter', kind: 'outbound', endpoint_url: 'https://git.example.com' };
    comp.createIntegration();
    fixture.detectChanges();
    expect(comp.backToTrigger()).toBeNull();
  });

  it('opens no create dialog for a trigger under read-only access', async () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false }, [], false, null, { task: 'demo', trigger: 't9' });
    await settled(fixture);
    expect(fixture.componentInstance.showCreateDialog()).toBe(false);
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
    expect(document.querySelector('.gr-dialog__error gr-message-banner')?.textContent).toContain('Name gitea-prod is already taken');
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

describe('IntegrationsComponent - validation', () => {
  const writable: AccessState = { managed: false, canEdit: true, canTrigger: true };
  const fieldErrors = () =>
    Array.from(document.querySelectorAll('.gr-dialog .field-error')).map((e) => e.textContent?.trim() ?? '');

  async function withWrites() {
    const fixture = setup(writable, [baseIntegration]);
    const writes = {
      createProjectIntegration: vi.fn(() => of(baseIntegration)),
      patchProjectIntegration: vi.fn(() => of(baseIntegration)),
    };
    Object.assign(TestBed.inject(IntegrationsService), writes);
    await settled(fixture);
    return { fixture, comp: fixture.componentInstance, writes };
  }

  it('refuses a name another integration of the same kind already uses', async () => {
    const { fixture, comp, writes } = await withWrites();
    comp.openCreateDialog();
    comp.formData.name = baseIntegration.name;
    comp.formData.kind = baseIntegration.kind as IntegrationsComponent['formData']['kind'];
    comp.createIntegration();
    fixture.detectChanges();
    expect(fieldErrors()).toContain('An integration of this kind with this name is already in this project.');
    expect(writes.createProjectIntegration).not.toHaveBeenCalled();
  });

  it('refuses source addresses that are not IP networks', async () => {
    const { fixture, comp, writes } = await withWrites();
    comp.openCreateDialog();
    comp.formData.name = 'other';
    comp.formData.allowed_ips = '10.0.0.0/8\nnot-an-ip';
    comp.createIntegration();
    fixture.detectChanges();
    expect(fieldErrors()).toContain('Not an IP address or CIDR range: not-an-ip');
    expect(writes.createProjectIntegration).not.toHaveBeenCalled();
  });

  it('shows an error for a renamed integration with an invalid name and sends nothing', async () => {
    const { fixture, comp, writes } = await withWrites();
    comp.openEditDialog(baseIntegration);
    comp.formData.name = 'Bad Name';
    comp.saveEdit();
    fixture.detectChanges();
    expect(fieldErrors()).toContain('Lowercase letters, digits and dashes only. Cannot start or end with a dash.');
    expect(writes.patchProjectIntegration).not.toHaveBeenCalled();
  });

  it('closes the edit dialog without an empty PATCH when nothing changed', async () => {
    const { comp, writes } = await withWrites();
    comp.openEditDialog(baseIntegration);
    comp.saveEdit();
    expect(writes.patchProjectIntegration).not.toHaveBeenCalled();
    expect(comp.showEditDialog()).toBe(false);
  });

  it('sends only the changed display name', async () => {
    const { comp, writes } = await withWrites();
    comp.openEditDialog(baseIntegration);
    comp.formData.display_name = 'Gitea Staging';
    comp.saveEdit();
    expect(writes.patchProjectIntegration).toHaveBeenCalledWith('acme', baseIntegration.id, { display_name: 'Gitea Staging' });
  });
});
