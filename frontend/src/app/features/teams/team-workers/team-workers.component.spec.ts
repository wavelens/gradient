/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { signal } from '@angular/core';
import { of } from 'rxjs';
import { TeamWorkersComponent } from './team-workers.component';
import { TeamsService } from '@core/services/teams.service';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { Team, TeamWorker } from '@core/models';

const team = { id: 't1', name: 'platform', display_name: 'Platform', managed: false, role: 'admin' } as Team;

const connection: TeamWorker = {
  worker_id: 'g1',
  display_name: 'Gradient.CI Servers',
  registered_at: '2026-10-05T12:00:00',
  active: true,
  managed: false,
  gradient_ci: true,
  enable_fetch: false,
  enable_eval: true,
  enable_build: true,
  connected: true,
};

function setup(role: 'admin' | 'member' = 'admin') {
  const updateWorker = vi.fn().mockReturnValue(of('updated'));
  TestBed.configureTestingModule({
    imports: [TeamWorkersComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: { snapshot: { paramMap: convertToParamMap({ team: 'platform' }) } } },
      { provide: AuthService, useValue: { user: signal({ superuser: false }) } },
      { provide: ConfigService, useValue: { gradientCiEnabled: true, gradientCiUrl: 'https://servers.gradient.ci' } },
      {
        provide: TeamsService,
        useValue: { get: () => of({ ...team, role }), workers: () => of([connection]), updateWorker },
      },
    ],
  });
  const fixture = TestBed.createComponent(TeamWorkersComponent);
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
      .find((b) => b.textContent?.includes('fetch'))!
      .dispatchEvent(new Event('click'));
    button(dialog, 'Save')!.click();

    expect(updateWorker).toHaveBeenCalledWith('platform', 'g1', { enable_fetch: true });
  });

  it('links a Gradient.CI connection to its site', () => {
    const { root } = setup('member');
    expect(root.querySelector('a[href="https://servers.gradient.ci"]')).not.toBeNull();
  });
});
