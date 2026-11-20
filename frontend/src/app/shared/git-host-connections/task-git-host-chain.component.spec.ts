/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { TaskGitHostChain } from '@core/models';
import { links } from '@shared/routing';
import { TaskGitHostChainComponent } from './task-git-host-chain.component';

const chain: TaskGitHostChain = {
  repository: { state: 'ok', detail: 'acme/app on git.example.com' },
  connection: { state: 'ok', detail: 'Forgejo' },
  incoming_events: { state: 'attention', detail: 'No event received yet', action: { type: 'install_webhook' } },
  commit_status: { state: 'off', detail: 'Off for this task', action: { type: 'turn_on_report_status' } },
};

function render(inputs: Record<string, unknown> = {}) {
  TestBed.configureTestingModule({ imports: [TaskGitHostChainComponent], providers: [provideRouter([])] });
  const fixture = TestBed.createComponent(TaskGitHostChainComponent);
  fixture.componentRef.setInput('chain', chain);
  fixture.componentRef.setInput('repositoryPageLink', links.taskPage('acme', 'app', 'repository'));
  for (const [name, value] of Object.entries(inputs)) fixture.componentRef.setInput(name, value);
  fixture.detectChanges();
  return fixture.nativeElement as HTMLElement;
}

describe('TaskGitHostChainComponent', () => {
  it('shows the four steps in order, each with its state beside the name', () => {
    const el = render();
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

  it('links only the first step that needs action to the repository page', () => {
    const el = render();
    const actions = el.querySelectorAll('[data-testid^="chain-action-"]');
    expect(actions).toHaveLength(1);
    const action = actions[0] as HTMLAnchorElement;
    expect(action.getAttribute('data-testid')).toBe('chain-action-install_webhook');
    expect(action.getAttribute('href')).toBe('/project/acme/task/app/repository');
  });

  it('links a webhook added by hand to the repository page too', () => {
    const el = render({
      chain: { ...chain, incoming_events: { ...chain.incoming_events, action: { type: 'add_webhook_on_host', webhook_url: 'https://ci.example.com/hook/1' } } },
    });
    expect(el.querySelector('[data-testid="chain-action-add_webhook_on_host"]')?.getAttribute('href')).toBe('/project/acme/task/app/repository');
    expect(el.querySelector('gr-copy-field')).toBeNull();
  });

  it('shows no link for an action this page cannot run', () => {
    const el = render({
      chain: { ...chain, connection: { state: 'attention', detail: "The GitHub App isn't installed on acme", action: { type: 'install_github_app', account: 'acme' } } },
    });
    expect(el.querySelector('[data-testid="chain-connection"] a')).toBeNull();
  });
});
