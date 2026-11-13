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
  it('splits the own connections from the inherited ones and names their owner', () => {
    const { el, list } = render();
    expect(list).toHaveBeenCalledWith(project);
    expect(rows(el, 'own-connection').map((row) => row.textContent)).toEqual([expect.stringContaining('Forgejo')]);
    const inheritedRows = rows(el, 'inherited-connection');
    expect(inheritedRows).toHaveLength(1);
    expect(inheritedRows[0].textContent).toContain('inherited');
    expect(inheritedRows[0].textContent).toContain('Inherited from Team platform');
    expect(inheritedRows[0].querySelector('button')).toBeNull();
  });

  it('names the own connections only when inherited ones are listed below them', () => {
    const headings = (el: HTMLElement) => Array.from(el.querySelectorAll('h2')).map((h) => h.textContent?.trim());
    expect(headings(render(writable, [own, inherited]).el)).toEqual(['Owned Connections', 'Inherited Connections']);
    TestBed.resetTestingModule();
    const { el } = render(writable, []);
    expect(headings(el)).toEqual(['No connections to Git hosts']);
    expect(rows(el, 'new-connection')).toHaveLength(1);
  });

  it('leaves the new connection button of a filled list to the page header', () => {
    const { el } = render(writable, [own]);
    expect(rows(el, 'new-connection')).toHaveLength(0);
  });

  it('creates a connection with PUT and shows the generated secret once', () => {
    const create = vi.fn(() => of({ ...full, generated_secret: 'whsec-123' }));
    const { fixture, component, notify } = render(writable, [own], { create });
    component.openCreate();
    component.patchForm({ name: 'gitea', git_host_type: 'gitea', allowed_ips: '10.0.0.0/8' });
    component.save();
    fixture.detectChanges();

    expect(create).toHaveBeenCalledWith(project, {
      name: 'gitea',
      git_host_type: 'gitea',
      receives_events: true,
      reports_status: false,
      allowed_ips: ['10.0.0.0/8'],
    });
    expect(notify.success).toHaveBeenCalledWith('Git host connection saved');
    const values = Array.from(document.querySelectorAll<HTMLInputElement>('.gr-dialog gr-copy-field input')).map((input) => input.value);
    expect(values).toEqual(['https://ci.example.com/hooks/c1', 'whsec-123']);
    (document.querySelector('[data-testid="secret-done"]') as HTMLButtonElement).click();
    expect(component.createdConnectionWithOneTimeSecret()).toBeNull();
  });

  it('loads the full connection to edit it and sends only the changed fields', () => {
    const get = vi.fn(() => of(full));
    const patch = vi.fn(() => of(full));
    const { component } = render(writable, [own], { get, patch });
    component.openEdit(own);
    expect(get).toHaveBeenCalledWith(project, 'c1');
    expect(component.form().name).toBe('forgejo');
    component.patchForm({ display_name: 'Forgejo Prod', reports_status: false });
    component.save();
    expect(patch).toHaveBeenCalledWith(project, 'c1', { display_name: 'Forgejo Prod', reports_status: false });
  });

  it('marks a connection whose access token refreshes automatically', () => {
    const get = vi.fn(() => of({ ...full, access_token_refreshes_automatically: true }));
    const { fixture, component } = render(writable, [own], { get });
    component.openEdit(own);
    fixture.detectChanges();
    expect(document.querySelector('.gr-dialog [data-testid="token-refreshes-badge"]')).not.toBeNull();
  });

  it('offers the OAuth2 connection only to a project', () => {
    const { el } = render();
    expect(el.querySelector('app-forgejo-oauth-connection')).not.toBeNull();
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [GitHostConnectionListComponent],
      providers: [provideRouter([]), { provide: GitHostConnectionsApi, useValue: { list: () => of([]) } }],
    });
    const team = TestBed.createComponent(GitHostConnectionListComponent);
    team.componentRef.setInput('gitHostConnectionOwner', { type: 'team', name: 'platform' });
    team.componentRef.setInput('access', writable);
    team.detectChanges();
    expect((team.nativeElement as HTMLElement).querySelector('app-forgejo-oauth-connection')).toBeNull();
  });

  it('removes a connection after the confirmation', () => {
    const remove = vi.fn(() => of('deleted'));
    const { fixture, el, confirm, notify } = render(writable, [own], { delete: remove });
    const deleteButton = rows(el, 'own-connection')[0].querySelector<HTMLButtonElement>('[aria-label="Delete Forgejo"]')!;
    deleteButton.click();
    fixture.detectChanges();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Delete git host connection' }));
    expect(remove).toHaveBeenCalledWith(project, 'c1');
    expect(notify.success).toHaveBeenCalledWith('Git host connection deleted');
    expect(rows(el, 'own-connection')).toHaveLength(0);
  });

  it('hides create, edit and delete from a member who cannot manage connections', () => {
    const { el } = render(readOnly, [{ ...own, can_manage: false }]);
    expect(el.querySelector('[data-testid="new-connection"]')).toBeNull();
    expect(el.querySelector('[aria-label="Edit Forgejo"]')).toBeNull();
    expect(el.querySelector('[aria-label="Delete Forgejo"]')).toBeNull();
    expect(el.querySelector('[aria-label="Deliveries of Forgejo"]')).not.toBeNull();
  });

  it('refuses a connection that neither receives events nor reports status', () => {
    const create = vi.fn();
    const { component } = render(writable, [], { create });
    component.openCreate();
    component.patchForm({ name: 'idle', receives_events: false, reports_status: false });
    expect(component.capabilityError()).toContain('Receives events');
    component.save();
    expect(create).not.toHaveBeenCalled();
  });

  it('asks for a base URL and token before a connection reports status', () => {
    const { component } = render(writable, []);
    component.openCreate();
    component.patchForm({ name: 'status', reports_status: true });
    expect(component.baseUrlError()).not.toBeNull();
    expect(component.accessTokenError()).toBe('Enter an access token to report status.');
  });
});
