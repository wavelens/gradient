/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { of } from 'rxjs';
import type { GitHostConnectionOwner, GitHostConnectionResponse, GitHostConnectionTestResponse } from '@core/models';
import { ConfigService } from '@core/services/config.service';
import { GitHostConnectionsApi } from '@core/services/git-host-connections.service';
import { NotifyService } from '@core/services/notify.service';
import { GitHostConnectionDialogComponent } from './git-host-connection-dialog.component';

const project: GitHostConnectionOwner = { type: 'project', name: 'acme' };

const saved: GitHostConnectionResponse = {
  id: 'c1',
  owner: project,
  name: 'git-example',
  display_name: 'git-example',
  git_host_type: 'forgejo',
  base_url: 'https://git.example.com',
  receives_events: true,
  reports_status: true,
  can_manage: true,
  access_token_refreshes_automatically: false,
  refused_signature_count: 0,
  task_count: 0,
  managed: false,
  has_secret: true,
  has_access_token: true,
  allowed_ips: [],
  webhook_url: 'https://ci.example.com/hooks/c1',
  created_by: 'u1',
  created_at: '2026-01-01T00:00:00Z',
};

function render(api: { create?: unknown; patch?: unknown; get?: unknown; test?: GitHostConnectionTestResponse } = {}) {
  const notify = { success: vi.fn(), failure: vi.fn() };
  const create = vi.fn(() => of(api.create ?? saved));
  const patch = vi.fn(() => of(api.patch ?? saved));
  const test = vi.fn(() => of(api.test ?? { success: true, account: 'ci-bot' }));
  TestBed.configureTestingModule({
    imports: [GitHostConnectionDialogComponent],
    providers: [
      { provide: GitHostConnectionsApi, useValue: { create, patch, test, get: vi.fn(() => of(api.get ?? saved)) } },
      { provide: NotifyService, useValue: notify },
      { provide: ConfigService, useValue: { giteaOauthAppConfig: null } },
    ],
  });
  const fixture = TestBed.createComponent(GitHostConnectionDialogComponent);
  fixture.componentRef.setInput('owner', project);
  fixture.componentRef.setInput('repository', 'https://git.example.com/acme/app');
  fixture.detectChanges();
  const component = fixture.componentInstance;
  const savedEvents: GitHostConnectionResponse[] = [];
  component.saved.subscribe((connection) => savedEvents.push(connection));
  const find = (testId: string) => document.querySelector<HTMLElement>(`[data-testid="${testId}"]`);
  return { fixture, component, notify, create, patch, test, find, savedEvents };
}

describe('GitHostConnectionDialogComponent', () => {
  it('creates a connection from the prefilled host, checks the token and skips manual Webhook setup', () => {
    const { fixture, component, create, test, find, savedEvents } = render();
    component.openCreate({ base_url: 'https://git.example.com/acme/app' });
    component.form.update((form) => ({ ...form, access_token: 'tok' }));
    component.save();
    fixture.detectChanges();
    expect(create).toHaveBeenCalledWith(project, expect.objectContaining({ name: 'git-example', base_url: 'https://git.example.com', access_token: 'tok' }));
    expect(test).toHaveBeenCalledWith(project, 'c1');
    expect(savedEvents).toEqual([saved]);
    expect(find('token-check')?.textContent).toContain('Signed in as ci-bot');
    expect(find('webhook-setup')).toBeNull();
  });

  it('shows Webhook settings with a one-time secret when no token value is present, with the dialog open until a press of Done', () => {
    const created = { ...saved, has_access_token: false, reports_status: false, generated_secret: 'whsec-123' };
    const { fixture, component, test, find } = render({ create: created });
    component.openCreate({ base_url: 'https://git.example.com' });
    component.save();
    fixture.detectChanges();
    expect(test).not.toHaveBeenCalled();
    const copied = Array.from(find('webhook-setup')!.querySelectorAll('input'), (input) => input.value);
    expect(copied).toEqual(['https://ci.example.com/hooks/c1', 'whsec-123']);
    expect(document.querySelector('a[href="https://git.example.com/acme/app/settings/hooks"]')).not.toBeNull();
    find('connection-done')!.click();
    fixture.detectChanges();
    expect(component.open()).toBe(false);
  });

  it('shows the host error and webhook settings if the token check fails', () => {
    const { fixture, component, find } = render({ test: { success: false, error: 'bad credentials' } });
    component.openCreate({ base_url: 'https://git.example.com' });
    component.form.update((form) => ({ ...form, access_token: 'tok' }));
    component.save();
    fixture.detectChanges();
    expect(find('token-check')?.textContent).toContain('bad credentials');
    expect(find('webhook-setup')).not.toBeNull();
  });

  it('stores edits and closes without a new token', () => {
    const { component, patch, notify, test } = render();
    component.openEdit(saved);
    component.form.update((form) => ({ ...form, display_name: 'Example' }));
    component.save();
    expect(patch).toHaveBeenCalledWith(project, 'c1', { display_name: 'Example' });
    expect(test).not.toHaveBeenCalled();
    expect(notify.success).toHaveBeenCalledWith('Git host connection saved');
    expect(component.open()).toBe(false);
  });

  it('starts each opening with Advanced collapsed', () => {
    const { fixture, component, find } = render();
    component.openEdit(saved);
    fixture.detectChanges();
    find('connection-advanced-toggle')!.click();
    fixture.detectChanges();
    expect(document.getElementById('connection-advanced')).not.toBeNull();
    component.setOpen(false);
    fixture.detectChanges();
    component.openEdit(saved);
    fixture.detectChanges();
    expect(document.getElementById('connection-advanced')).toBeNull();
  });

  it('opens Advanced when internal settings differ from defaults', () => {
    const { fixture, component } = render({ get: { ...saved, allowed_ips: ['10.0.0.0/8'] } });
    component.openEdit(saved);
    fixture.detectChanges();
    expect(document.getElementById('connection-advanced')).not.toBeNull();
  });

  it('opens Advanced with the Webhook secret field marked as the focus target on a secret change request', () => {
    const { fixture, component } = render();
    component.openEdit(saved, { focusWebhookSecret: true });
    fixture.detectChanges();
    expect(document.getElementById('connection-secret')?.hasAttribute('cdkFocusInitial')).toBe(true);
  });

  it('shows the Webhook URL and the new secret after an edit that changes the secret', () => {
    const { fixture, component, find, notify } = render();
    component.openEdit(saved, { focusWebhookSecret: true });
    component.form.update((form) => ({ ...form, secret: 'whsec-new' }));
    component.save();
    fixture.detectChanges();
    expect(notify.success).not.toHaveBeenCalled();
    const copied = Array.from(find('webhook-setup')!.querySelectorAll('input'), (input) => input.value);
    expect(copied).toEqual(['https://ci.example.com/hooks/c1', 'whsec-new']);
    expect(component.open()).toBe(true);
  });

  it('does not send anything and shows errors for an incomplete form', () => {
    const { fixture, component, create } = render();
    component.openCreate();
    component.save();
    fixture.detectChanges();
    expect(create).not.toHaveBeenCalled();
    expect(document.querySelector('.gr-dialog')?.textContent).toContain('Enter the Host URL.');
  });
});
