/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { of } from 'rxjs';
import type { AccessState, GitHostConnectionOwner, GitHostConnectionResponse, GitHostConnectionSummary } from '@core/models';
import { GitHostConnectionsApi } from '@core/services/git-host-connections.service';
import { NotifyService } from '@core/services/notify.service';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
import { ConfigService } from '@core/services/config.service';
import { GitHostConnectionListComponent } from './git-host-connection-list.component';

const project: GitHostConnectionOwner = { type: 'project', name: 'acme' };
const writable: AccessState = { managed: false, canEdit: true, canTrigger: true };
const readOnly: AccessState = { managed: false, canEdit: false, canTrigger: false };

const own: GitHostConnectionSummary = {
  id: 'c1',
  owner: project,
  name: 'forgejo',
  display_name: 'Forgejo',
  git_host_type: 'forgejo',
  base_url: 'https://forgejo.example.com',
  receives_events: true,
  reports_status: true,
  can_manage: true,
  access_token_refreshes_automatically: false,
  refused_signature_count: 0,
  task_count: 0,
};

const inherited: GitHostConnectionSummary = {
  ...own,
  id: 'c2',
  owner: { type: 'team', name: 'platform' },
  name: 'team-gitlab',
  display_name: 'Team GitLab',
  git_host_type: 'gitlab',
  can_manage: false,
};

const full: GitHostConnectionResponse = {
  ...own,
  managed: false,
  has_secret: true,
  has_access_token: true,
  access_token_refreshes_automatically: false,
  allowed_ips: [],
  webhook_url: 'https://ci.example.com/hooks/c1',
  created_by: 'u1',
  created_at: '2026-01-01T00:00:00Z',
};

function render(
  access: AccessState = writable,
  connections: GitHostConnectionSummary[] = [own, inherited],
  api: object = {},
  inputs: Record<string, unknown> = {},
) {
  const notify = { success: vi.fn(), failure: vi.fn(), copyToClipboard: vi.fn(() => Promise.resolve()) };
  const confirm = vi.fn((request: ConfirmRequest) => request.action());
  const list = vi.fn(() => of(connections));
  TestBed.configureTestingModule({
    imports: [GitHostConnectionListComponent],
    providers: [
      provideRouter([]),
      { provide: GitHostConnectionsApi, useValue: { list, listDeliveries: () => of([]), ...api } },
      { provide: NotifyService, useValue: notify },
      { provide: ConfirmService, useValue: { confirm } },
      { provide: ConfigService, useValue: { giteaOauthAppConfig: null } },
    ],
  });
  const fixture = TestBed.createComponent(GitHostConnectionListComponent);
  fixture.componentRef.setInput('gitHostConnectionOwner', project);
  fixture.componentRef.setInput('access', access);
  for (const [name, value] of Object.entries(inputs)) fixture.componentRef.setInput(name, value);
  fixture.detectChanges();
  const el = fixture.nativeElement as HTMLElement;
  return { fixture, el, component: fixture.componentInstance, notify, confirm, list };
}

function rows(el: HTMLElement, testId: string): HTMLElement[] {
  return Array.from(el.querySelectorAll<HTMLElement>(`[data-testid="${testId}"]`));
}

describe('GitHostConnectionListComponent', () => {
  const installation: GitHostConnectionSummary = {
    ...own,
    id: 'c3',
    name: 'github-acme',
    display_name: 'GitHub',
    git_host_type: 'github',
    account_login: 'acme',
    github_installation_id: 42,
  };

  it('lists only the connections of the chosen git host types', () => {
    const { el } = render(writable, [own, installation], {}, { gitHostTypes: ['github'] });
    const names = Array.from(el.querySelectorAll('.connection-row')).map((row) => row.textContent);
    expect(names).toHaveLength(1);
    expect(names[0]).toContain('acme');
  });

  it('shows nothing for an empty list without an empty state', () => {
    const { el } = render(writable, [own], {}, { gitHostTypes: ['github'], showsEmptyState: false });
    expect(el.querySelector('gr-empty-state')).toBeNull();
    expect(el.querySelector('.connection-row')).toBeNull();
  });

  it('marks a connection shown on request and loads the list again', () => {
    const { fixture, el, component, list } = render(writable, [own, installation]);
    component.showConnection('c3');
    fixture.detectChanges();
    expect(list).toHaveBeenCalledTimes(2);
    expect(el.querySelector('#connection-c3')?.classList).toContain('connection-row-linked');
  });

  it('lists the own connections first and names the owner of inherited ones without actions', () => {
    const { el, list } = render(writable, [inherited, own]);
    expect(list).toHaveBeenCalledWith(project);
    const all = Array.from(el.querySelectorAll<HTMLElement>('.connection-row'));
    expect(all.map((row) => row.dataset['testid'])).toEqual(['own-connection', 'inherited-connection']);
    expect(all[1].textContent).toContain('Team platform');
    expect(all[1].querySelector('.connection-actions button')).toBeNull();
    expect(el.querySelectorAll('h2').length).toBe(0);
  });

  it('shows the health of a connection in one line', () => {
    const healthy: GitHostConnectionSummary = {
      ...own,
      access_token_refreshes_automatically: true,
      refused_signature_count: 3,
      task_count: 2,
      last_delivery: { id: 'd1', created_at: '2026-01-01T00:00:00Z', result: 'queued', queued: 1, status_code: 200 } as never,
    };
    const healthLine = render(writable, [healthy]).el.querySelector('[data-testid="connection-health"]')!;
    const health = healthLine.textContent!;
    expect(healthLine.querySelector('time.connection-delivery-time')?.getAttribute('datetime')).toBe('2026-01-01T00:00:00Z');
    expect(health).toContain('Queued 1 evaluation');
    expect(health).toContain('Token refreshes automatically');
    expect(health).toContain('3 refused signatures');
    expect(health).toContain('2 tasks');
  });

  it('shows a refused signature newer than the last delivery as the latest event', () => {
    const refusedLast: GitHostConnectionSummary = {
      ...own,
      refused_signature_count: 2,
      last_refused_signature_at: '2026-01-02T00:00:00Z',
      last_delivery: { id: 'd1', created_at: '2026-01-01T00:00:00Z', result: 'queued', queued: 1, status_code: 200 } as never,
    };
    const healthLine = render(writable, [refusedLast]).el.querySelector('[data-testid="connection-health"]')!;
    expect(healthLine.textContent).toContain('Refused a delivery with a wrong signature');
    expect(healthLine.textContent).not.toContain('Queued 1 evaluation');
    expect(healthLine.querySelector('time.connection-delivery-time')?.getAttribute('datetime')).toBe('2026-01-02T00:00:00Z');
  });

  it('shows a refused signature instead of no events yet', () => {
    const health = render(writable, [{ ...own, refused_signature_count: 1, last_refused_signature_at: '2026-01-02T00:00:00Z' }]).el
      .querySelector('[data-testid="connection-health"]')!.textContent!;
    expect(health).toContain('Refused a delivery with a wrong signature');
    expect(health).not.toContain('No events yet');
  });

  it('names a single refused signature in the singular', () => {
    const health = render(writable, [{ ...own, refused_signature_count: 1 }]).el
      .querySelector('[data-testid="connection-health"]')!.textContent!;
    expect(health).toContain('1 refused signature');
    expect(health).not.toContain('1 refused signatures');
  });

  it('expands the tasks of a connection with links to their Repository page', () => {
    const listTasks = vi.fn(() => of([{ id: 't1', project: 'acme', name: 'app', display_name: 'App', repository: 'r', report_status: true }]));
    const { fixture, el } = render(writable, [{ ...own, task_count: 1 }], { listTasks });
    el.querySelector<HTMLButtonElement>('[data-testid="connection-tasks"]')!.click();
    fixture.detectChanges();
    expect(listTasks).toHaveBeenCalledWith(project, 'c1');
    expect(el.querySelector('[data-testid="connection-task-list"] a')?.getAttribute('href')).toBe('/project/acme/task/app/repository');
  });

  it('offers a new connection in the empty state only', () => {
    expect(rows(render(writable, []).el, 'new-connection')).toHaveLength(1);
    TestBed.resetTestingModule();
    expect(rows(render(writable, [inherited]).el, 'new-connection')).toHaveLength(0);
  });

  it('tests a connection and reports the result as a toast', () => {
    const test = vi.fn(() => of({ success: true, account: 'ci-bot' }));
    const { el, component, notify } = render(writable, [own], { test });
    el.querySelector<HTMLButtonElement>('[aria-label="Actions for Forgejo"]')!.click();
    component.menuItems().find((item) => item.label === 'Test')!.command!();
    expect(test).toHaveBeenCalledWith(project, 'c1');
    expect(notify.success).toHaveBeenCalledWith('Forgejo: signed in as ci-bot');
  });

  it('opens the edit dialog with the full connection', () => {
    const get = vi.fn(() => of(full));
    const { el } = render(writable, [own], { get });
    el.querySelector<HTMLButtonElement>('[aria-label="Edit Forgejo"]')!.click();
    expect(get).toHaveBeenCalledWith(project, 'c1');
  });

  it('removes a connection from the actions menu after the confirmation', () => {
    const remove = vi.fn(() => of(true));
    const { fixture, el, component, confirm } = render(writable, [own], { delete: remove });
    component.remove(own);
    fixture.detectChanges();
    expect(confirm).toHaveBeenCalled();
    expect(remove).toHaveBeenCalledWith(project, 'c1');
    expect(rows(el, 'own-connection')).toHaveLength(0);
    expect(el.querySelector('[aria-label="Actions for Forgejo"]')).toBeNull();
  });

  it('hides edit and the actions menu from a member who cannot manage connections', () => {
    const { el } = render(readOnly, [own]);
    expect(el.querySelector('[aria-label="Edit Forgejo"]')).toBeNull();
    expect(el.querySelector('[aria-label="Actions for Forgejo"]')).toBeNull();
    expect(el.querySelector('[aria-label="Deliveries of Forgejo"]')).not.toBeNull();
  });

  it('tests but never edits a GitHub App connection', () => {
    const { el } = render(writable, [{ ...own, git_host_type: 'github', display_name: 'GitHub' }]);
    expect(el.querySelector('[aria-label="Actions for GitHub"]')).not.toBeNull();
    expect(el.querySelector('[aria-label="Edit GitHub"]')).toBeNull();
  });

  it('tests but never edits or deletes a connection of declarative state', () => {
    const { el, component } = render(writable, [{ ...own, managed: true }]);
    expect(el.querySelector('[aria-label="Edit Forgejo"]')).toBeNull();
    el.querySelector<HTMLButtonElement>('[aria-label="Actions for Forgejo"]')!.click();
    expect(component.menuItems().map((item) => item.label)).toEqual(['Test']);
  });

  it('badges the event of the last delivery by its outcome', () => {
    const failed = {
      ...own,
      last_delivery: { id: 'd1', created_at: '2026-01-01T00:00:00Z', event: 'push', result: 'queued', queued: 1, status_code: 500 } as never,
    };
    const badge = render(writable, [failed]).el.querySelector('[data-testid="connection-health"] gr-badge');
    expect(badge?.textContent?.trim()).toBe('push');
    expect(badge?.querySelector('.badge')?.classList).toContain('is-danger');
  });

  it('places the opened task list right below the tasks toggle', () => {
    const listTasks = vi.fn(() => of([]));
    const { fixture, el } = render(writable, [{ ...own, task_count: 1 }], { listTasks });
    el.querySelector<HTMLButtonElement>('[data-testid="connection-tasks"]')!.click();
    fixture.detectChanges();
    const list = el.querySelector('[data-testid="connection-task-list"]')!;
    expect(list.parentElement?.classList).toContain('connection-summary');
    expect(list.previousElementSibling?.getAttribute('data-testid')).toBe('connection-health');
  });

  it('copies the webhook URL from the actions menu and shows it only in the deliveries panel', () => {
    const withUrl = { ...own, webhook_url: 'https://ci.example.com/hooks/c1' };
    const { fixture, el, component, notify } = render(readOnly, [withUrl]);
    expect(el.querySelector('gr-copy-field')).toBeNull();
    el.querySelector<HTMLButtonElement>('[aria-label="Actions for Forgejo"]')!.click();
    expect(component.menuItems().map((item) => item.label)).toEqual(['Copy webhook URL']);
    component.menuItems()[0].command!();
    expect(notify.copyToClipboard).toHaveBeenCalledWith('https://ci.example.com/hooks/c1', 'webhook URL');
    el.querySelector<HTMLButtonElement>('[aria-label="Deliveries of Forgejo"]')!.click();
    fixture.detectChanges();
    const field = el.querySelector('gr-copy-field');
    expect(field).not.toBeNull();
    expect(field!.nextElementSibling?.tagName.toLowerCase()).toBe('app-incoming-deliveries');
  });

  it('offers no webhook URL for a GitHub App connection', () => {
    const { el } = render(readOnly, [{ ...own, git_host_type: 'github', display_name: 'GitHub', webhook_url: 'https://ci.example.com/hooks/c1' }]);
    expect(el.querySelector('[aria-label="Actions for GitHub"]')).toBeNull();
  });

  it('shows the OAuth result banner on a project page only', () => {
    expect(render().el.querySelector('app-git-host-oauth-return-banner')).not.toBeNull();
  });
});
