/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, provideRouter } from '@angular/router';
import { taskAccessData } from '@core/resolvers/access-route.testing';
import { of, throwError } from 'rxjs';
import { RepositoryIntegration, TaskRepositoryResponse } from '@core/models';
import { TasksService } from '@core/services/tasks.service';
import { ApiError } from '@core/services/api.service';
import { IntegrationsService } from '@core/services/integrations.service';
import { ConfigService } from '@core/services/config.service';
import { AuthService } from '@core/services/auth.service';
import { RepositoryEventsComponent } from './repository-events.component';

const inbound: RepositoryIntegration = {
  id: 'in1',
  name: 'forgejo-example-com',
  display_name: 'forgejo.example.com',
  kind: 'inbound',
  git_host_type: 'forgejo',
  serves_repository: true,
  webhook_url: 'https://ci.example.com/api/v1/hooks/forgejo/acme/forgejo-example-com',
  has_secret: true,
  has_access_token: null,
};

const outbound: RepositoryIntegration = { ...inbound, id: 'out1', kind: 'outbound', webhook_url: null, has_access_token: true };

function repo(overrides: Partial<TaskRepositoryResponse> = {}): TaskRepositoryResponse {
  return {
    repository: 'ssh://git@forgejo.example.com/acme/app.git',
    uses_ssh: true,
    deploy_key: 'ssh-ed25519 AAAA',
    fetch: null,
    build_on_push: null,
    build_on_pull_request: null,
    last_event: null,
    report_status: null,
    integrations: [],
    can_edit: true,
    can_manage_integrations: true,
    ...overrides,
  };
}

function render(state: TaskRepositoryResponse, services: { tasks?: object; integrations?: object; githubApp?: boolean; superuser?: boolean } = {}, managed = false) {
  TestBed.configureTestingModule({
    imports: [RepositoryEventsComponent],
    providers: [
      provideRouter([]),
      { provide: ActivatedRoute, useValue: { parent: { data: of(taskAccessData({ managed, canEdit: !managed, canTrigger: !managed })) } } },
      { provide: TasksService, useValue: services.tasks ?? {} },
      { provide: IntegrationsService, useValue: services.integrations ?? { listIntegrationDeliveries: () => of([]) } },
      { provide: ConfigService, useValue: { githubAppEnabled: services.githubApp ?? false } },
      { provide: AuthService, useValue: { user: () => ({ superuser: services.superuser ?? false }) } },
    ],
  });
  const fixture = TestBed.createComponent(RepositoryEventsComponent);
  fixture.componentRef.setInput('project', 'acme');
  fixture.componentRef.setInput('task', 'app');
  fixture.componentRef.setInput('state', state);
  fixture.detectChanges();
  return fixture;
}

describe('RepositoryEventsComponent', () => {
  it('offers to connect the host when no connection serves the repository', () => {
    const el = render(repo()).nativeElement as HTMLElement;
    expect(el.querySelector('[data-testid="connect-git-host"]')?.textContent).toContain('forgejo.example.com');
  });

  it('points a GitHub repository to the next step for the GitHub App', () => {
    const github = repo({ repository: 'https://github.com/acme/app' });
    const hint = (options: { githubApp?: boolean; superuser?: boolean }) => {
      TestBed.resetTestingModule();
      const el = render(github, options).nativeElement as HTMLElement;
      return el.querySelector('[data-testid="github-hint"]');
    };
    expect(hint({ githubApp: true })?.querySelector('a')?.getAttribute('href')).toBe('/project/acme/integrations');
    expect(hint({ superuser: true })?.querySelector('a')?.getAttribute('href')).toBe('/admin/github-app');
    const asked = hint({});
    expect(asked?.querySelector('a')).toBeNull();
    expect(asked?.textContent).toContain('Ask an instance admin');
  });

  it('tells members without integration rights whom to ask', () => {
    const el = render(repo({ can_manage_integrations: false })).nativeElement as HTMLElement;
    expect(el.querySelector('[data-testid="ask-admin"]')?.textContent).toContain('Ask a project admin');
  });

  it('shows view-only members the state without a save button', () => {
    const el = render(repo({ can_edit: false, can_manage_integrations: false, integrations: [inbound] })).nativeElement as HTMLElement;
    expect(el.querySelector('[data-testid="read-only"]')).not.toBeNull();
    expect(el.textContent).not.toContain('Save');
  });

  it('names the server configuration as the reason a managed task is read-only, not a missing role', () => {
    const el = render(repo({ can_edit: false, integrations: [inbound] }), {}, true).nativeElement as HTMLElement;
    expect(el.querySelector('[data-testid="read-only"]')?.textContent).toContain('managed by the server configuration');
  });

  it('turns on push and pull request builds when a connection is picked and saves them', () => {
    const updateRepository = vi.fn(() => of(repo()));
    const fixture = render(repo({ integrations: [inbound, outbound] }), {
      tasks: { updateRepository },
      integrations: { listIntegrationDeliveries: () => of([]) },
    });
    const comp = fixture.componentInstance;
    comp.patch({ inbound: null });
    comp.patch({ inbound: 'in1' });
    expect(comp.form().push).toBe(true);
    expect(comp.form().pullRequest).toBe(true);
    comp.patch({ outbound: null, reportStatus: false });
    comp.patch({ outbound: 'out1' });
    comp.save();
    expect(updateRepository).toHaveBeenCalledWith('acme', 'app', expect.objectContaining({
      inbound_integration: 'in1',
      build_on_push: true,
      build_on_pull_request: true,
      outbound_integration: 'out1',
      report_status: true,
    }));
  });

  it('connects a Git host, shows the secret when the webhook install fails and wires push, pull requests and status', () => {
    const connectGitHost = vi.fn(() => of({
      inbound: { ...inbound, generated_secret: 's3cret' },
      outbound: { ...outbound },
    }));
    const updateRepository = vi.fn(() => of(repo()));
    const installWebhook = vi.fn(() => throwError(() => new ApiError('403 token lacks admin rights', 502)));
    const fixture = render(repo(), {
      tasks: { updateRepository, installWebhook, installDeployKey: () => of('acme/app') },
      integrations: { connectGitHost, listIntegrationDeliveries: () => of([]) },
    });
    const comp = fixture.componentInstance;
    comp.openConnect();
    expect(comp.connectForm.endpoint_url).toBe('https://forgejo.example.com');
    comp.connectForm.access_token = 'token';
    comp.connect();
    expect(connectGitHost).toHaveBeenCalledWith('acme', expect.objectContaining({
      name: 'forgejo-example-com',
      git_host_type: 'forgejo',
      access_token: 'token',
    }));
    expect(installWebhook).toHaveBeenCalledWith('acme', 'app', { inbound_integration: 'in1', outbound_integration: 'out1' });
    expect(comp.createdSecret()?.secret).toBe('s3cret');
    expect(comp.showSecret()).toBe(true);
    expect(updateRepository).toHaveBeenCalledWith('acme', 'app', expect.objectContaining({
      inbound_integration: 'in1',
      build_on_push: true,
      outbound_integration: 'out1',
      report_status: true,
    }));
  });

  it('installs the webhook and deploy key through the Git host and skips the manual secret', () => {
    const connectGitHost = vi.fn(() => of({ inbound: { ...inbound, generated_secret: 's3cret' }, outbound: { ...outbound } }));
    const installWebhook = vi.fn(() => of('acme/app'));
    const installDeployKey = vi.fn(() => of('acme/app'));
    const fixture = render(repo(), {
      tasks: { updateRepository: () => of(repo()), installWebhook, installDeployKey },
      integrations: { connectGitHost, listIntegrationDeliveries: () => of([]) },
    });
    const comp = fixture.componentInstance;
    comp.openConnect();
    comp.connectForm.access_token = 'token';
    comp.connect();
    expect(installWebhook).toHaveBeenCalled();
    expect(installDeployKey).toHaveBeenCalledWith('acme', 'app', { outbound_integration: 'out1' });
    expect(comp.showSecret()).toBe(false);
  });

  it('offers to install the webhook only with a token on a connection that serves the repository', () => {
    const withToken = render(repo({ integrations: [inbound, outbound] })).nativeElement as HTMLElement;
    expect(withToken.querySelector('[data-testid="install-webhook"]')).not.toBeNull();
    TestBed.resetTestingModule();
    const withoutToken = render(repo({ integrations: [inbound, { ...outbound, has_access_token: false }] })).nativeElement as HTMLElement;
    expect(withoutToken.querySelector('[data-testid="install-webhook"]')).toBeNull();
  });
});
