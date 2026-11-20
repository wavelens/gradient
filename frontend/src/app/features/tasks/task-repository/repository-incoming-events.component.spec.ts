/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { of } from 'rxjs';
import { GitHostConnectionSummary, IncomingWebhookDeliveryResponse, TaskGitHostChain, TaskRepositoryResponse } from '@core/models';
import { TasksService } from '@core/services/tasks.service';
import { NotifyService } from '@core/services/notify.service';
import { ConfigService } from '@core/services/config.service';
import { GitHostConnectionsApi } from '@core/services/git-host-connections.service';
import { RepositoryIncomingEventsComponent } from './repository-incoming-events.component';

const connection: GitHostConnectionSummary = {
  id: 'c1',
  owner: { type: 'project', name: 'acme' },
  name: 'forgejo',
  display_name: 'Forgejo',
  git_host_type: 'forgejo',
  base_url: 'https://forgejo.example.com',
  receives_events: true,
  reports_status: true,
  can_manage: true,
  webhook_url: 'https://ci.example.com/api/v1/hooks/c1',
  access_token_refreshes_automatically: false,
  refused_signature_count: 0,
  task_count: 1,
};

const delivery = (id: string, at: string, result: IncomingWebhookDeliveryResponse['result']): IncomingWebhookDeliveryResponse => ({
  id,
  event: 'push',
  status_code: 200,
  result,
  repository: 'https://forgejo.example.com/acme/app',
  tasks_scanned: 1,
  queued: result === 'queued' ? 1 : 0,
  created_at: at,
});

const waiting: TaskGitHostChain = {
  repository: { state: 'ok', detail: 'acme/app on forgejo.example.com' },
  connection: { state: 'ok', detail: 'Forgejo' },
  incoming_events: { state: 'attention', detail: 'No event received yet', action: { type: 'install_webhook' } },
  commit_status: { state: 'attention', detail: 'No status posted yet' },
};

const state: TaskRepositoryResponse = {
  repository: 'https://forgejo.example.com/acme/app.git',
  uses_ssh: false,
  deploy_key: null,
  fetch: null,
  git_host_connection: connection,
  report_status: true,
  recent_deliveries: [],
  chain: waiting,
  status_posts: [],
  available_connections: [connection],
  can_edit: true,
  can_manage_integrations: true,
};

function render(overrides: Partial<TaskRepositoryResponse> = {}, tasks: object = {}, hasPrimaryAction = false) {
  const notify = { success: vi.fn(), failure: vi.fn() };
  TestBed.configureTestingModule({
    imports: [RepositoryIncomingEventsComponent],
    providers: [
      { provide: TasksService, useValue: tasks },
      { provide: NotifyService, useValue: notify },
      { provide: GitHostConnectionsApi, useValue: {} },
      { provide: ConfigService, useValue: { giteaOauthAppConfig: null } },
    ],
  });
  const fixture = TestBed.createComponent(RepositoryIncomingEventsComponent);
  fixture.componentRef.setInput('project', 'acme');
  fixture.componentRef.setInput('task', 'app');
  fixture.componentRef.setInput('state', { ...state, ...overrides });
  fixture.componentRef.setInput('hasPrimaryAction', hasPrimaryAction);
  fixture.detectChanges();
  return { fixture, el: fixture.nativeElement as HTMLElement, component: fixture.componentInstance, notify };
}

describe('RepositoryIncomingEventsComponent', () => {
  it('heads the section with the incoming events step and its state', () => {
    const { el } = render();
    expect(el.querySelector('h2')?.textContent).toBe('Incoming events');
    expect(el.querySelector('.settings-section__heading app-chain-step-state-badge')?.textContent).toContain('Incomplete');
    expect(el.querySelector('[data-testid="step-status"]')?.textContent).toContain('No event received yet');
  });

  it('shows only its state while the connection step blocks it', () => {
    const chain = { ...waiting, connection: { state: 'attention' as const, detail: 'No connection' }, incoming_events: { state: 'off' as const, detail: 'Needs a connection' } };
    const { el } = render({ chain, git_host_connection: null });
    expect(el.querySelector('.settings-section__body')?.textContent?.trim()).toBe('Needs a connection');
  });

  it('installs the webhook through the server with the primary button and reloads', () => {
    const installWebhook = vi.fn(() => of('acme/app'));
    const { el, component, notify } = render({}, { installWebhook }, true);
    const changed = vi.fn();
    component.changed.subscribe(changed);
    const install = el.querySelector('[data-testid="install-webhook"]') as HTMLElement;
    expect(install.classList).toContain('gr-button--primary');
    install.click();
    expect(installWebhook).toHaveBeenCalledWith('acme', 'app');
    expect(notify.success).toHaveBeenCalledWith('Webhook installed');
    expect(changed).toHaveBeenCalled();
  });

  it('gives the webhook URL and the host webhook settings when the webhook must be added by hand', () => {
    const chain = { ...waiting, incoming_events: { ...waiting.incoming_events, action: { type: 'add_webhook_on_host' as const, webhook_url: 'https://ci.example.com/api/v1/hooks/c1' } } };
    const { el } = render({ chain, can_manage_integrations: false });
    expect((el.querySelector('gr-copy-field input') as HTMLInputElement).value).toBe('https://ci.example.com/api/v1/hooks/c1');
    expect(el.querySelector('a[href="https://forgejo.example.com/acme/app/settings/hooks"]')).not.toBeNull();
    expect(el.querySelector('[data-testid="install-webhook"]')).toBeNull();
  });

  it('opens the connection with the webhook secret field focused when the host signs with another secret', () => {
    const chain = { ...waiting, incoming_events: { state: 'failing' as const, detail: 'Wrong signature', action: { type: 'change_webhook_secret' as const } } };
    const full = { ...connection, has_secret: true, has_access_token: true, allowed_ips: [], managed: false, created_by: 'u1', created_at: '2026-01-01T00:00:00Z' };
    const get = vi.fn(() => of(full));
    TestBed.overrideProvider(GitHostConnectionsApi, { useValue: { get } });
    const { fixture, el } = render({ chain }, {}, true);
    const change = el.querySelector('[data-testid="change-webhook-secret"]') as HTMLElement;
    expect(change.classList).toContain('gr-button--primary');
    change.click();
    fixture.detectChanges();
    expect(get).toHaveBeenCalledWith(connection.owner, 'c1');
    expect(document.getElementById('connection-secret')?.hasAttribute('cdkFocusInitial')).toBe(true);
  });

  it('shows the last delivery with its time and the earlier deliveries below', () => {
    const chain = {
      ...waiting,
      incoming_events: { state: 'attention' as const, detail: 'push event matched no task: app has no trigger for push events', at: '2026-11-20T10:00:00' },
    };
    const recent_deliveries = [delivery('d2', '2026-11-20T10:00:00', 'no_matching_task'), delivery('d1', '2026-11-20T09:00:00', 'queued')];
    const { el } = render({ chain, recent_deliveries });
    expect(el.querySelector('[data-testid="step-status"]')?.textContent).toContain('app has no trigger for push events');
    const history = Array.from(el.querySelectorAll('app-step-history-list li'));
    expect(history).toHaveLength(1);
    expect(history[0].textContent).toContain('Queued 1 evaluation');
  });
});
