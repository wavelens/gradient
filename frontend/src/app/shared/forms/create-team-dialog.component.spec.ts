/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { Subject, of, throwError } from 'rxjs';
import { ApiError } from '@core/services/api.service';
import { TeamsService } from '@core/services/teams.service';
import { CreateTeamDialogComponent } from './create-team-dialog.component';

type Exposed = { form: { name: string; displayName: string }; create(): void; creating(): boolean };

function setup(create: () => unknown) {
  TestBed.configureTestingModule({
    imports: [CreateTeamDialogComponent],
    providers: [{ provide: TeamsService, useValue: { create: vi.fn(create), checkTeamNameAvailable: () => of(true) } }],
  });
  const fixture = TestBed.createComponent(CreateTeamDialogComponent);
  fixture.componentInstance.visible.set(true);
  fixture.detectChanges();
  const dialog = fixture.componentInstance as unknown as Exposed;
  return { fixture, dialog, teams: TestBed.inject(TeamsService) as unknown as { create: ReturnType<typeof vi.fn> } };
}

describe('CreateTeamDialogComponent', () => {
  it('reports the created team by name and closes', () => {
    const { fixture, dialog } = setup(() => of('id'));
    const created = vi.fn();
    fixture.componentInstance.created.subscribe(created);
    dialog.form = { name: 'ops', displayName: 'Ops' };
    dialog.create();
    expect(created).toHaveBeenCalledWith('ops');
    expect(fixture.componentInstance.visible()).toBe(false);
  });

  it('keeps a refused creation inside the dialog', () => {
    const { fixture, dialog } = setup(() => throwError(() => new ApiError('Team name already taken', 409)));
    dialog.form = { name: 'platform', displayName: 'Platform' };
    dialog.create();
    fixture.detectChanges();
    expect(fixture.componentInstance.visible()).toBe(true);
    expect(dialog.creating()).toBe(false);
    expect(document.querySelector('.gr-dialog')?.textContent).toContain('Team name already taken');
  });

  it('asks before Cancel drops typed input', async () => {
    const { fixture } = setup(() => of('id'));
    const input = document.querySelector<HTMLInputElement>('#team-display-name')!;
    input.value = 'Ops';
    input.dispatchEvent(new Event('input'));
    await fixture.whenStable();
    fixture.detectChanges();
    Array.from(document.querySelectorAll<HTMLButtonElement>('button')).find((b) => b.textContent?.includes('Cancel'))!.click();
    fixture.detectChanges();
    expect(fixture.componentInstance.visible()).toBe(true);
    expect(document.querySelector('.gr-dialog__discard-question')).not.toBeNull();
  });

  it('sends one request while the creation runs', () => {
    const { fixture, dialog, teams } = setup(() => new Subject());
    dialog.form = { name: 'ops', displayName: 'Ops' };
    dialog.create();
    fixture.detectChanges();
    dialog.create();
    expect(teams.create).toHaveBeenCalledTimes(1);
    expect(fixture.componentInstance.visible()).toBe(true);
  });
});
