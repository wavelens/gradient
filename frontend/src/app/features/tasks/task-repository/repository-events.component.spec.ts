/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { of } from 'rxjs';
import { RepositoryIntegration, TaskRepositoryResponse } from '@core/models';
import { TasksService } from '@core/services/tasks.service';
import { IntegrationsService } from '@core/services/integrations.service';
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

function render(state: TaskRepositoryResponse, services: { tasks?: object; integrations?: object } = {}) {
  TestBed.configureTestingModule({
    imports: [RepositoryEventsComponent],
    providers: [
      provideRouter([]),
      { provide: TasksService, useValue: services.tasks ?? {} },
      { provide: IntegrationsService, useValue: services.integrations ?? { listIntegrationDeliveries: () => of([]) } },
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

  it('tells members without integration rights whom to ask', () => {
    const el = render(repo({ can_manage_integrations: false })).nativeElement as HTMLElement;
    expect(el.querySelector('[data-testid="ask-admin"]')?.textContent).toContain('Ask a project admin');
  });

  it('shows view-only members the state without a save button', () => {
    const el = render(repo({ can_edit: false, can_manage_integrations: false, integrations: [inbound] })).nativeElement as HTMLElement;
    expect(el.querySelector('[data-testid="read-only"]')).not.toBeNull();
    expect(el.textContent).not.toContain('Save');
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

  it('connects a Git host, shows the secret once and wires push, pull requests and status', () => {
    const connectGitHost = vi.fn(() => of({
      inbound: { ...inbound, generated_secret: 's3cret' },
      outbound: { ...outbound },
    }));
    const updateRepository = vi.fn(() => of(repo()));
    const fixture = render(repo(), {
      tasks: { updateRepository },
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
    expect(comp.createdSecret()?.secret).toBe('s3cret');
    expect(updateRepository).toHaveBeenCalledWith('acme', 'app', expect.objectContaining({
      inbound_integration: 'in1',
      build_on_push: true,
      outbound_integration: 'out1',
      report_status: true,
    }));
  });
});
