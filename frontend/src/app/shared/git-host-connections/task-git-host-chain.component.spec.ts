/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { IncomingWebhookDeliveryResponse, TaskGitHostChain, TaskGitHostChainStepAction } from '@core/models';
import { links } from '@shared/routing';
import { TaskGitHostChainComponent } from './task-git-host-chain.component';

const chain: TaskGitHostChain = {
  repository: { state: 'ok', detail: 'acme/app on git.example.com' },
  connection: { state: 'ok', detail: 'Forgejo' },
  incoming_events: { state: 'attention', detail: 'No event received yet', action: { type: 'install_webhook' } },
  commit_status: { state: 'off', detail: 'Off for this task', action: { type: 'turn_on_report_status' } },
};

const delivery: IncomingWebhookDeliveryResponse = {
  id: 'd1',
  event: 'push',
  status_code: 200,
  result: 'queued',
  queued: 1,
  tasks_scanned: 1,
  created_at: '2026-11-20T10:00:00',
};

function render(inputs: Record<string, unknown> = {}) {
  TestBed.configureTestingModule({ imports: [TaskGitHostChainComponent], providers: [provideRouter([])] });
  const fixture = TestBed.createComponent(TaskGitHostChainComponent);
  fixture.componentRef.setInput('chain', chain);
  fixture.componentRef.setInput('repository', 'https://git.example.com/acme/app.git');
  fixture.componentRef.setInput('gitHostType', 'forgejo');
  for (const [name, value] of Object.entries(inputs)) fixture.componentRef.setInput(name, value);
  const emitted: TaskGitHostChainStepAction[] = [];
  fixture.componentInstance.action.subscribe((action) => emitted.push(action));
  fixture.detectChanges();
  return { fixture, el: fixture.nativeElement as HTMLElement, emitted };
}

describe('TaskGitHostChainComponent', () => {
  it('shows the four steps in order, each with its state beside the name', () => {
    const { el } = render();
    const rows = Array.from(el.querySelectorAll('gr-row'));
    expect(rows.map((row) => row.getAttribute('data-testid'))).toEqual([
      'chain-repository',
      'chain-connection',
      'chain-incoming_events',
      'chain-commit_status',
    ]);
    expect(rows[2].textContent).toContain('Incoming events');
    expect(rows[2].textContent).toContain('Incomplete');
    expect(rows[2].textContent).toContain('No event received yet');
  });

  it('emits the action of a step when its button is pressed', () => {
    const { el, emitted } = render();
    (el.querySelector('[data-testid="chain-action-install_webhook"]') as HTMLButtonElement).click();
    expect(emitted).toEqual([{ type: 'install_webhook' }]);
  });

  it('offers only the action of the first step that needs one', () => {
    const { el } = render();
    expect(el.querySelectorAll('[data-testid^="chain-action-"]')).toHaveLength(1);
    expect(el.querySelector('[data-testid="chain-action-turn_on_report_status"]')).toBeNull();
  });

  it('links actions to the repository page when a link is given', () => {
    const { el, emitted } = render({ repositoryPageLink: links.taskPage('acme', 'app', 'repository') });
    const action = el.querySelector('[data-testid="chain-action-install_webhook"]') as HTMLAnchorElement;
    expect(action.tagName).toBe('A');
    expect(action.getAttribute('href')).toBe('/project/acme/task/app/repository');
    expect(emitted).toEqual([]);
  });

  it('shows the webhook URL and the host webhook settings for a webhook added by hand', () => {
    const { el } = render({
      chain: { ...chain, incoming_events: { ...chain.incoming_events, action: { type: 'add_webhook_on_host', webhook_url: 'https://ci.example.com/hook/1' } } },
    });
    const row = el.querySelector('[data-testid="chain-incoming_events"]') as HTMLElement;
    expect((row.querySelector('gr-copy-field input') as HTMLInputElement).value).toBe('https://ci.example.com/hook/1');
    expect(row.querySelector('a[href="https://git.example.com/acme/app/settings/hooks"]')).not.toBeNull();
  });

  it('shows no button for an action this page cannot run', () => {
    const { el } = render({
      chain: { ...chain, connection: { state: 'attention', detail: "The GitHub App isn't installed on acme", action: { type: 'install_github_app', account: 'acme' } } },
    });
    expect(el.querySelector('[data-testid="chain-connection"] button')).toBeNull();
  });

  it('expands the delivery history of the events step', () => {
    const { el, fixture } = render({ deliveries: [delivery] });
    expect(el.querySelector('[data-testid="chain-history"]')).toBeNull();
    (el.querySelector('[data-testid="chain-incoming_events"] button[aria-expanded]') as HTMLButtonElement).click();
    fixture.detectChanges();
    expect(el.querySelector('[data-testid="chain-history"]')?.textContent).toContain('Queued 1 evaluation');
  });
});
