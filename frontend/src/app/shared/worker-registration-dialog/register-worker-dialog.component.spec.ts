/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { Observable, Subject, of, throwError } from 'rxjs';
import type { WorkerRegistrationResponse } from '@core/models';
import { ApiError } from '@core/services/api.service';
import { TeamsService } from '@core/services/teams.service';
import { WorkersService } from '@core/services/workers.service';
import { RegisterWorkerDialogComponent } from './register-worker-dialog.component';

const registration: WorkerRegistrationResponse = {
  id: 'w-new',
  name: 'Builder',
  url: 'https://gradient.example.com/project/demo/workers',
  peer_id: 'peer-uuid',
  token: 'one-time-token',
  nixos_configuration: 'services.gradient.worker = {\n  enable = true;\n};\n',
};

function setup(owner: { project?: string; team?: string }, answer: () => Observable<WorkerRegistrationResponse> = () => of(registration)) {
  const workers = { registerWorker: vi.fn(answer) };
  const teams = { registerWorker: vi.fn(answer) };
  TestBed.configureTestingModule({
    imports: [RegisterWorkerDialogComponent],
    providers: [
      provideRouter([]),
      { provide: WorkersService, useValue: workers },
      { provide: TeamsService, useValue: teams },
    ],
  });
  const fixture = TestBed.createComponent(RegisterWorkerDialogComponent);
  if (owner.project) fixture.componentRef.setInput('project', owner.project);
  if (owner.team) fixture.componentRef.setInput('team', owner.team);
  fixture.componentInstance.visible.set(true);
  fixture.detectChanges();
  return { fixture, workers, teams };
}

function button(label: string): HTMLButtonElement | undefined {
  return Array.from(document.querySelectorAll<HTMLButtonElement>('button')).find(
    (b) => b.querySelector('.gr-button__label')?.textContent?.trim() === label,
  );
}

async function typeDisplayName(fixture: ReturnType<typeof setup>['fixture'], value: string) {
  const input = document.querySelector<HTMLInputElement>('#register-worker-display-name')!;
  input.value = value;
  input.dispatchEvent(new Event('input'));
  await fixture.whenStable();
  fixture.detectChanges();
}

describe('RegisterWorkerDialogComponent', () => {
  it('registers a project worker without a worker ID or token and shows the NixOS configuration and the peers line once', async () => {
    const { fixture, workers } = setup({ project: 'demo' });
    const registered = vi.fn();
    fixture.componentInstance.workerRegistered.subscribe(registered);
    await typeDisplayName(fixture, ' Builder ');
    button('Register')!.click();
    fixture.detectChanges();

    expect(workers.registerWorker).toHaveBeenCalledWith('demo', {
      display_name: 'Builder',
      url: undefined,
      enable_network: true,
      enable_secrets: true,
      enable_exec: false,
      enable_eval: true,
      enable_build: true,
    });
    expect(registered).toHaveBeenCalledWith(registration);
    expect(fixture.componentInstance.visible()).toBe(false);

    const result = document.querySelector('.gr-dialog')!;
    const copied = Array.from(result.querySelectorAll<HTMLInputElement | HTMLTextAreaElement>('gr-copy-field input, gr-copy-field textarea'), (field) => field.value);
    expect(copied).toEqual([registration.nixos_configuration, 'peer-uuid:one-time-token']);
    expect(result.querySelector('.gr-dialog__close')).toBeNull();
    expect(result.querySelector('a[href="/project/demo/workers/w-new/metrics"]')).not.toBeNull();

    button("I've saved the token")!.click();
    fixture.detectChanges();
    expect(document.body.textContent).not.toContain('one-time-token');
  });

  it('registers a team worker on the team endpoint', async () => {
    const { fixture, workers, teams } = setup({ team: 'platform' });
    await typeDisplayName(fixture, 'Builder');
    button('Register')!.click();
    fixture.detectChanges();
    expect(teams.registerWorker).toHaveBeenCalledWith('platform', expect.objectContaining({ display_name: 'Builder' }));
    expect(workers.registerWorker).not.toHaveBeenCalled();
    expect(document.querySelector<HTMLInputElement>('.gr-dialog gr-copy-field input')?.value).toBe('peer-uuid:one-time-token');
  });

  it('explains the exec capability next to the capability toggles', () => {
    setup({ project: 'demo' });
    expect(document.querySelector('.gr-dialog')?.textContent).toContain('exec can start exec jobs on this worker');
  });

  it('names a missing display name and sends nothing', () => {
    const { fixture, workers } = setup({ project: 'demo' });
    button('Register')!.click();
    fixture.detectChanges();
    expect(document.querySelector('.gr-dialog')?.textContent).toContain('Enter a display name.');
    expect(workers.registerWorker).not.toHaveBeenCalled();
  });

  it('keeps a refused registration inside the dialog', async () => {
    const { fixture } = setup({ project: 'demo' }, () => throwError(() => new ApiError('Display name is too long', 422)));
    await typeDisplayName(fixture, 'Builder');
    button('Register')!.click();
    fixture.detectChanges();
    expect(fixture.componentInstance.visible()).toBe(true);
    expect(document.querySelector('.gr-dialog')?.textContent).toContain('Display name is too long');
  });

  it('sends one request and stays open while registration runs', async () => {
    const { fixture, workers } = setup({ project: 'demo' }, () => new Subject());
    await typeDisplayName(fixture, 'Builder');
    button('Register')!.click();
    fixture.detectChanges();
    button('Register')!.click();
    (document.querySelector('.gr-dialog-backdrop') as HTMLElement).click();
    fixture.detectChanges();
    expect(workers.registerWorker).toHaveBeenCalledTimes(1);
    expect(fixture.componentInstance.visible()).toBe(true);
    expect(button('Cancel')!.disabled).toBe(true);
  });
});
