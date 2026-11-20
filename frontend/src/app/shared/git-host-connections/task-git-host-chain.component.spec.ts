/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';
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

const summary = (el: HTMLElement) => el.querySelector('[data-testid="chain-summary"]')?.textContent ?? '';

describe('TaskGitHostChainComponent', () => {
  it('sums the chain up in one line by its worst step', () => {
    const el = render();
    expect(summary(el)).toContain('Incomplete');
    expect(summary(el)).toContain('Incoming events');
    expect(summary(el)).toContain('No event received yet');
    expect(el.querySelectorAll('button')).toHaveLength(0);
  });

  it('puts a failing step before an earlier incomplete one', () => {
    const el = render({ chain: { ...chain, commit_status: { state: 'failing', detail: 'The host refused the status' } } });
    expect(summary(el)).toContain('Commit status');
    expect(summary(el)).toContain('The host refused the status');
  });

  it('shows the first step when every step works', () => {
    const working = { ...chain, incoming_events: { state: 'ok' as const, detail: 'push queued 1 evaluation' }, commit_status: { state: 'ok' as const, detail: 'Posted' } };
    const el = render({ chain: working });
    expect(summary(el)).toContain('Working');
    expect(summary(el)).toContain('Repository');
  });

  it('always links to the repository page', () => {
    const el = render({ chain: { ...chain, incoming_events: { state: 'ok' as const, detail: 'ok' }, commit_status: { state: 'ok' as const, detail: 'ok' } } });
    expect(el.querySelector('[data-testid="open-repository"]')?.getAttribute('href')).toBe('/project/acme/task/app/repository');
  });

  it('closes the given dialog before it opens the repository page', () => {
    const close = vi.fn();
    const el = render({ dialogToCloseFirst: { close } });
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    el.querySelector<HTMLElement>('[data-testid="open-repository"]')!.click();
    expect(navigate).not.toHaveBeenCalled();
    close.mock.calls[0][0]();
    expect(navigate).toHaveBeenCalledWith(links.taskPage('acme', 'app', 'repository'));
  });
});
