/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { signal } from '@angular/core';
import { EMPTY, Observable, of, throwError } from 'rxjs';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
import { ApiError } from '@core/services/api.service';
import { TeamWorkersComponent } from './team-workers.component';
import { TeamsService } from '@core/services/teams.service';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { NotifyService } from '@core/services/notify.service';
import { TeamResponse, TeamWorkerEntry } from '@core/models';
import { MenuItem } from '@gradient/ui/ui';

const team = { id: 't1', name: 'platform', display_name: 'Platform', managed: false, role: 'admin' } as TeamResponse;

const connection: TeamWorkerEntry = {
  worker_id: 'g1',
  display_name: 'Gradient.CI Servers',
  registered_at: '2026-10-05T12:00:00',
  active: true,
  managed: false,
  gradient_ci: true,
  enable_network: true,
  enable_secrets: false,
  enable_exec: false,
  enable_eval: true,
  enable_build: true,
  connected: true,
  requested_by_admin: false,
};

const live = { capabilities: {}, architectures: [], system_features: [], max_concurrent_builds: 2, assigned_job_count: 1, draining: false } as unknown as NonNullable<TeamWorkerEntry['live']>;

const offline = { ...connection, worker_id: 'w2', display_name: 'Sleeper', gradient_ci: false, connected: false };

function setup(
  role: 'admin' | 'member' = 'admin',
  workers: TeamWorkerEntry[] = [connection],
  list: () => Observable<TeamWorkerEntry[]> = () => of(workers),
) {
  const updateWorker = vi.fn().mockReturnValue(of('updated'));
  const registerWorker = vi.fn().mockReturnValue(
    of({ id: 'w9', name: 'Builder', url: '', peer_id: 'team-uuid', token: 'secret', nixos_configuration: 'services.gradient.worker = {};' }),
  );
  const removeWorker = vi.fn().mockReturnValue(of('removed'));
  const notify = { success: vi.fn(), failure: vi.fn() };
  const confirm = vi.fn((request: ConfirmRequest): Observable<unknown> => request.action());
  TestBed.configureTestingModule({
    imports: [TeamWorkersComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: AuthService, useValue: { user: signal({ superuser: false }) } },
      { provide: ConfigService, useValue: { gradientCiEnabled: true, gradientCiUrl: 'https://servers.gradient.ci' } },
      { provide: TeamsService, useValue: { workers: list, updateWorker, registerWorker, removeWorker } },
      { provide: NotifyService, useValue: notify },
      { provide: ConfirmService, useValue: { confirm } },
    ],
  });
  const fixture = TestBed.createComponent(TeamWorkersComponent);
  fixture.componentRef.setInput('team', { ...team, role });
  fixture.detectChanges();
  return { fixture, updateWorker, registerWorker, removeWorker, notify, confirm, root: fixture.nativeElement as HTMLElement };
}

function button(root: Element, label: string): HTMLButtonElement | undefined {
  return (Array.from(root.querySelectorAll('button')) as HTMLButtonElement[]).find(
    (b) => b.querySelector('.gr-button__label')?.textContent?.trim() === label,
  );
}

function findMenuItemByLabelInAllTeamWorkerMenus(fixture: ComponentFixture<TeamWorkersComponent>, label: string): MenuItem | undefined {
  return [...fixture.componentInstance.workerMenus().values()].flat().find((item) => item.label === label);
}

describe('TeamWorkersComponent', () => {
  it('shows members what a worker is allowed without letting them change it', () => {
    const { root } = setup('member');
    expect(root.querySelector('.allowed-capability-chip')).not.toBeNull();
    expect(root.querySelector('[aria-label="Worker actions"]')).toBeNull();
  });

  it('shows every worker as a worker card with its status and source', () => {
    const { root } = setup('admin', [{ ...connection, live }]);
    const card = root.querySelector('app-worker-card-list > app-worker-card')!;
    expect(card.querySelector('.worker-source')?.textContent).toContain('Team worker');
    expect(card.querySelector('.worker-card__status-pills-cell')?.textContent).toContain('Connected');
    expect(card.querySelector('.job-count')?.textContent?.trim()).toBe('1 / 2');
    expect(card.querySelector('[aria-label="Worker actions"]')).not.toBeNull();
  });

  it('shows a lock instead of Edit and Delete on a worker set in the server configuration', () => {
    const { fixture, root } = setup('admin', [{ ...connection, managed: true }]);
    expect(root.querySelector('app-server-configuration-managed-lock-button')).not.toBeNull();
    expect(findMenuItemByLabelInAllTeamWorkerMenus(fixture, 'Edit')).toBeUndefined();
    expect(findMenuItemByLabelInAllTeamWorkerMenus(fixture, 'Delete')).toBeUndefined();
    expect(findMenuItemByLabelInAllTeamWorkerMenus(fixture, 'Deactivate')).toBeDefined();
  });

  it('hides the Workers section from members while the team has no workers, but not from admins', () => {
    expect(setup('member', []).root.textContent).not.toContain('Workers');
    TestBed.resetTestingModule();
    expect(setup('admin', []).root.textContent).toContain('Workers');
  });

  it('saves what a Gradient.CI connection is allowed from its Edit dialog', () => {
    const { fixture, updateWorker } = setup();

    findMenuItemByLabelInAllTeamWorkerMenus(fixture, 'Edit')!.command!();
    fixture.detectChanges();
    const dialog = document.querySelector('.gr-dialog')!;
    Array.from(dialog.querySelectorAll('gr-allowed-capabilities button'))
      .find((b) => b.textContent?.includes('secrets'))!
      .dispatchEvent(new Event('click'));
    button(dialog, 'Save')!.click();

    expect(updateWorker).toHaveBeenCalledWith('platform', 'g1', { enable_secrets: true });
  });

  it('registers a team worker through the shared dialog and reloads the list', async () => {
    const list = vi.fn(() => of([connection]));
    const { fixture, root, registerWorker } = setup('admin', [connection], list);
    button(root, 'Register Worker')!.click();
    fixture.detectChanges();
    await fixture.whenStable();
    const dialog = document.querySelector('.gr-dialog')!;
    const name = dialog.querySelector<HTMLInputElement>('#register-worker-display-name')!;
    name.value = 'Builder';
    name.dispatchEvent(new Event('input'));
    button(dialog, 'Register')!.click();
    fixture.detectChanges();

    expect(root.querySelector('app-register-worker-dialog')).not.toBeNull();
    expect(registerWorker).toHaveBeenCalledWith('platform', expect.objectContaining({ display_name: 'Builder' }));
    expect(registerWorker.mock.calls[0][1]).not.toHaveProperty('worker_id');
    expect(list).toHaveBeenCalledTimes(2);
    expect(document.querySelector<HTMLInputElement>('.gr-dialog gr-copy-field input')?.value).toBe('team-uuid:secret');
  });

  it('closes the Edit dialog without a request when nothing changed', () => {
    const { fixture, updateWorker } = setup();
    findMenuItemByLabelInAllTeamWorkerMenus(fixture, 'Edit')!.command!();
    fixture.detectChanges();
    button(document.querySelector('.gr-dialog')!, 'Save')!.click();
    expect(updateWorker).not.toHaveBeenCalled();
    expect(fixture.componentInstance.editing()).toBeNull();
  });

  it('keeps a failed edit inside the Edit dialog', () => {
    const { fixture, root, updateWorker } = setup();
    updateWorker.mockReturnValue(throwError(() => new ApiError('Worker is managed', 409)));
    findMenuItemByLabelInAllTeamWorkerMenus(fixture, 'Edit')!.command!();
    fixture.componentInstance.editAllowed = { ...connection, enable_secrets: true };
    fixture.detectChanges();
    button(document.querySelector('.gr-dialog')!, 'Save')!.click();
    fixture.detectChanges();
    expect(fixture.componentInstance.editing()).not.toBeNull();
    expect(document.querySelector('.gr-dialog')?.textContent).toContain('Worker is managed');
    expect(root.textContent).not.toContain('Worker is managed');
  });

  it('asks before deactivating a worker, since it aborts running jobs', () => {
    const { fixture, updateWorker, notify, confirm } = setup();
    findMenuItemByLabelInAllTeamWorkerMenus(fixture, 'Deactivate')!.command!();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Disable Worker', target: 'Gradient.CI Servers' }));
    expect(updateWorker).toHaveBeenCalledWith('platform', 'g1', { active: false });
    expect(notify.success).toHaveBeenCalledWith('Worker deactivated');
  });

  it('sends no deactivation when the confirmation is cancelled', () => {
    const { fixture, updateWorker, confirm } = setup();
    confirm.mockImplementation(() => EMPTY);
    findMenuItemByLabelInAllTeamWorkerMenus(fixture, 'Deactivate')!.command!();
    expect(updateWorker).not.toHaveBeenCalled();
  });

  it('asks first, then drains a connected worker', () => {
    const { fixture, updateWorker, notify, confirm } = setup('admin', [{ ...connection, live }]);
    findMenuItemByLabelInAllTeamWorkerMenus(fixture, 'Drain')!.command!();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Worker Drain', target: 'Gradient.CI Servers' }));
    expect(updateWorker).toHaveBeenCalledWith('platform', 'g1', { draining: true });
    expect(notify.success).toHaveBeenCalledWith('Drain started');
  });

  it('offers Undrain on an admin drain and shows the Draining badge', () => {
    const { fixture, root, updateWorker, notify, confirm } = setup('admin', [{ ...connection, live: { ...live, draining: true }, requested_by_admin: true }]);
    expect(root.querySelector('app-worker-drain-badge')?.textContent).toContain('Draining');
    expect(findMenuItemByLabelInAllTeamWorkerMenus(fixture, 'Drain')).toBeUndefined();
    findMenuItemByLabelInAllTeamWorkerMenus(fixture, 'Undrain')!.command!();
    expect(confirm).not.toHaveBeenCalled();
    expect(updateWorker).toHaveBeenCalledWith('platform', 'g1', { draining: false });
    expect(notify.success).toHaveBeenCalledWith('Drain stopped');
  });

  it('offers no drain on a worker that drains itself or is offline', () => {
    expect(findMenuItemByLabelInAllTeamWorkerMenus(setup('admin', [{ ...connection, live: { ...live, draining: true } }]).fixture, 'Drain')).toBeUndefined();
    TestBed.resetTestingModule();
    expect(findMenuItemByLabelInAllTeamWorkerMenus(setup('admin', [connection]).fixture, 'Drain')).toBeUndefined();
  });

  it('asks before deleting a worker, naming it, then confirms the deletion', () => {
    const { fixture, removeWorker, notify, confirm } = setup();
    findMenuItemByLabelInAllTeamWorkerMenus(fixture, 'Delete')!.command!();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Delete worker', target: 'Gradient.CI Servers' }));
    expect(removeWorker).toHaveBeenCalledWith('platform', 'g1');
    expect(notify.success).toHaveBeenCalledWith('Worker deleted');
  });

  it('sends no delete when the confirmation is cancelled', () => {
    const { fixture, removeWorker, confirm } = setup();
    confirm.mockImplementation(() => EMPTY);
    fixture.componentInstance.remove(connection);
    expect(removeWorker).not.toHaveBeenCalled();
  });

  it('hides offline workers until asked to show them', () => {
    const { fixture, root } = setup('admin', [connection, offline]);
    expect(root.textContent).toContain('Gradient.CI Servers');
    expect(root.textContent).not.toContain('Sleeper');

    button(root, 'Show Offline (1)')!.click();
    fixture.detectChanges();
    expect(root.textContent).toContain('Sleeper');
  });

  it.each([
    ['only offline workers', [offline]],
    ['no workers', []],
  ])('says no worker is online with %s', (_, workers) => {
    const { root } = setup('admin', workers);
    expect(root.textContent).toContain('No online workers');
  });

  it('links a Gradient.CI connection to its site', () => {
    const { root } = setup('member');
    expect(root.querySelector('a[href="https://servers.gradient.ci"]')).not.toBeNull();
  });

  it('shows a failed worker load with Try Again, never as no online workers', () => {
    let fail = true;
    const { fixture, root } = setup('member', [connection], () =>
      fail ? throwError(() => new ApiError('database is down', 500)) : of([connection]),
    );
    expect(root.textContent).toContain('Failed to load workers');
    expect(root.textContent).not.toContain('No online workers');

    fail = false;
    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')!.click();
    fixture.detectChanges();
    expect(root.textContent).toContain('Gradient.CI Servers');
  });
});
