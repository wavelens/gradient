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

function render(access: AccessState = writable, connections: GitHostConnectionSummary[] = [own, inherited], api: object = {}) {
  const notify = { success: vi.fn(), failure: vi.fn() };
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
  fixture.detectChanges();
  const el = fixture.nativeElement as HTMLElement;
  return { fixture, el, component: fixture.componentInstance, notify, confirm, list };
}

function rows(el: HTMLElement, testId: string): HTMLElement[] {
  return Array.from(el.querySelectorAll<HTMLElement>(`[data-testid="${testId}"]`));
}

describe('GitHostConnectionListComponent', () => {
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
    const health = render(writable, [healthy]).el.querySelector('[data-testid="connection-health"]')!.textContent!;
    expect(health).toContain('Queued 1 evaluation');
    expect(health).toContain('Token refreshes automatically');
    expect(health).toContain('3 refused signatures');
    expect(health).toContain('2 tasks');
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
    const { el, notify } = render(writable, [own], { test });
    el.querySelector<HTMLButtonElement>('[aria-label="Test Forgejo"]')!.click();
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

  it('hides test, edit and the actions menu from a member who cannot manage connections', () => {
    const { el } = render(readOnly, [own]);
    expect(el.querySelector('[aria-label="Test Forgejo"]')).toBeNull();
    expect(el.querySelector('[aria-label="Edit Forgejo"]')).toBeNull();
    expect(el.querySelector('[aria-label="Actions for Forgejo"]')).toBeNull();
    expect(el.querySelector('[aria-label="Deliveries of Forgejo"]')).not.toBeNull();
  });

  it('tests but never edits a GitHub App connection', () => {
    const { el } = render(writable, [{ ...own, git_host_type: 'github', display_name: 'GitHub' }]);
    expect(el.querySelector('[aria-label="Test GitHub"]')).not.toBeNull();
    expect(el.querySelector('[aria-label="Edit GitHub"]')).toBeNull();
  });

  it('shows the OAuth result banner on a project page only', () => {
    expect(render().el.querySelector('app-git-host-oauth-return-banner')).not.toBeNull();
  });
});
