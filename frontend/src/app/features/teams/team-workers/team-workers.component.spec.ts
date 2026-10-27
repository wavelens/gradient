/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { signal } from '@angular/core';
import { of } from 'rxjs';
import { TeamWorkersComponent } from './team-workers.component';
import { TeamsService } from '@core/services/teams.service';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { TeamResponse, TeamWorkerEntry } from '@core/models';

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
};

const offline = { ...connection, worker_id: 'w2', display_name: 'Sleeper', gradient_ci: false, connected: false };

function setup(role: 'admin' | 'member' = 'admin', workers: TeamWorkerEntry[] = [connection]) {
  const updateWorker = vi.fn().mockReturnValue(of('updated'));
  TestBed.configureTestingModule({
    imports: [TeamWorkersComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: AuthService, useValue: { user: signal({ superuser: false }) } },
      { provide: ConfigService, useValue: { gradientCiEnabled: true, gradientCiUrl: 'https://servers.gradient.ci' } },
      { provide: TeamsService, useValue: { workers: () => of(workers), updateWorker } },
    ],
  });
  const fixture = TestBed.createComponent(TeamWorkersComponent);
  fixture.componentRef.setInput('team', { ...team, role });
  fixture.detectChanges();
  return { fixture, updateWorker, root: fixture.nativeElement as HTMLElement };
}

function button(root: Element, label: string): HTMLButtonElement | undefined {
  return (Array.from(root.querySelectorAll('button')) as HTMLButtonElement[]).find(
    (b) => b.querySelector('.gr-button__label')?.textContent?.trim() === label,
  );
}

describe('TeamWorkersComponent', () => {
  it('shows members what a worker is allowed without letting them change it', () => {
    const { root } = setup('member');
    expect(root.textContent).toContain('Allowed:');
    expect(button(root, 'Edit')).toBeUndefined();
  });

  it('saves what a Gradient.CI connection is allowed from its Edit dialog', () => {
    const { fixture, root, updateWorker } = setup();

    button(root, 'Edit')!.click();
    fixture.detectChanges();
    const dialog = document.querySelector('.gr-dialog')!;
    Array.from(dialog.querySelectorAll('gr-allowed-capabilities button'))
      .find((b) => b.textContent?.includes('secrets'))!
      .dispatchEvent(new Event('click'));
    button(dialog, 'Save')!.click();

    expect(updateWorker).toHaveBeenCalledWith('platform', 'g1', { enable_secrets: true });
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
    const { root } = setup('member', workers);
    expect(root.textContent).toContain('No online workers');
  });

  it('links a Gradient.CI connection to its site', () => {
    const { root } = setup('member');
    expect(root.querySelector('a[href="https://servers.gradient.ci"]')).not.toBeNull();
  });
});
