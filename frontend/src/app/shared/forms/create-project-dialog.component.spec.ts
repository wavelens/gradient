/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { Subject, of, throwError } from 'rxjs';
import { ProjectsService } from '@core/services/projects.service';
import { TeamsService } from '@core/services/teams.service';
import { ApiError } from '@core/services/api.service';
import { NotifyService } from '@core/services/notify.service';
import type { TeamNameAndDisplayName, TeamSummary } from '@core/models';
import { CreateProjectDialogComponent } from './create-project-dialog.component';

@Component({
  selector: 'test-host',
  standalone: true,
  imports: [CreateProjectDialogComponent],
  template: `<app-create-project-dialog [(visible)]="visible" [preselectedOwnerTeam]="preselected" (created)="created.push($event)" />`,
})
class HostComponent {
  visible = true;
  preselected: TeamNameAndDisplayName | null = null;
  created: string[] = [];
}

const INFRA: TeamSummary = { name: 'infra', display_name: 'Infra', role: 'member' };
const WEB: TeamSummary = { name: 'web', display_name: 'Web', role: 'admin' };

async function setup(createProject: () => unknown, options: { teams?: TeamSummary[]; preselected?: TeamNameAndDisplayName; available?: boolean } = {}) {
  TestBed.configureTestingModule({
    providers: [
      { provide: ProjectsService, useValue: { createProject, checkProjectNameAvailable: () => of(options.available ?? true) } },
      { provide: TeamsService, useValue: { list: () => of(options.teams ?? []) } },
    ],
  });
  const fixture = TestBed.createComponent(HostComponent);
  fixture.componentInstance.preselected = options.preselected ?? null;
  const settle = async () => {
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
  };
  await settle();
  const button = (label: string) =>
    Array.from(document.querySelectorAll<HTMLButtonElement>('[grDialogFooter] button')).find((b) => b.querySelector('.gr-button__label')?.textContent?.trim() === label)!;
  const type = async (selector: string, value: string) => {
    const input = document.querySelector<HTMLInputElement>(selector)!;
    input.value = value;
    input.dispatchEvent(new Event('input'));
    await new Promise((resolve) => setTimeout(resolve, 450));
    await settle();
  };
  const submit = async () => {
    button('Create').click();
    await settle();
  };
  const cancel = async () => {
    button('Cancel').click();
    await settle();
  };
  const pickOwnerTeam = async (label: string) => {
    document.querySelector<HTMLButtonElement>('#project-owner-team')!.click();
    await settle();
    Array.from(document.querySelectorAll<HTMLElement>('.gr-select__option')).find((option) => option.textContent?.trim() === label)!.click();
    await settle();
  };
  const ownerTeamField = () => document.querySelector('#project-owner-team');
  return { host: fixture.componentInstance, type, submit, cancel, pickOwnerTeam, ownerTeamField };
}

describe('CreateProjectDialogComponent', () => {
  it('creates the project, closes, confirms with a toast and reports its name', async () => {
    const createProject = vi.fn(() => of('id'));
    const { host, type, submit } = await setup(createProject);
    const success = vi.spyOn(TestBed.inject(NotifyService), 'success');
    await type('#project-display-name', 'Acme');
    await submit();
    expect(createProject).toHaveBeenCalledWith(expect.objectContaining({ name: 'acme', display_name: 'Acme', public: false, owner_team: null }));
    expect(success).toHaveBeenCalledWith('Project created');
    expect(host.created).toEqual(['acme']);
    expect(host.visible).toBe(false);
  });

  it('names the missing fields when Create is clicked on an empty form', async () => {
    const createProject = vi.fn(() => of('id'));
    const { host, submit } = await setup(createProject);
    await submit();
    expect(createProject).not.toHaveBeenCalled();
    expect(host.visible).toBe(true);
    expect(document.body.textContent).toContain('Enter a display name.');
    expect(document.body.textContent).toContain('Enter a name.');
  });

  it('stays open with the error when creation fails', async () => {
    const { host, type, submit } = await setup(() => throwError(() => new ApiError('Project name acme is taken', 409)));
    await type('#project-display-name', 'Acme');
    await submit();
    expect(host.visible).toBe(true);
    expect(document.querySelector('.gr-dialog gr-message-banner')?.textContent).toContain('Project name acme is taken');
  });

  it('sends one request while creation runs', async () => {
    const pending = new Subject<string>();
    const createProject = vi.fn(() => pending);
    const { host, type, submit } = await setup(createProject);
    await type('#project-display-name', 'Acme');
    await submit();
    await submit();
    expect(createProject).toHaveBeenCalledTimes(1);
    expect(host.visible).toBe(true);
    pending.next('id');
    pending.complete();
  });

  it('closes on Cancel without asking while only the default owner team is chosen', async () => {
    const { host, cancel } = await setup(() => of('id'), { teams: [INFRA] });
    await cancel();
    expect(host.visible).toBe(false);
  });

  it('asks before Cancel drops typed input', async () => {
    const { host, type, cancel } = await setup(() => of('id'), { teams: [INFRA] });
    await type('#project-display-name', 'Acme');
    await cancel();
    expect(host.visible).toBe(true);
    expect(document.querySelector('.gr-dialog__discard-question')?.textContent).toContain('Discard changes?');
  });

  it('hides the owner team field for users without a team', async () => {
    const { ownerTeamField } = await setup(() => of('id'));
    expect(ownerTeamField()).toBeNull();
  });

  it('preselects the only team of the user as owner team', async () => {
    const createProject = vi.fn(() => of('id'));
    const { type, submit, ownerTeamField } = await setup(createProject, { teams: [INFRA] });
    expect(ownerTeamField()?.textContent).toContain('Infra (infra)');
    await type('#project-display-name', 'Acme');
    await submit();
    expect(createProject).toHaveBeenCalledWith(expect.objectContaining({ owner_team: 'infra' }));
  });

  it('leaves the owner team empty when the user belongs to several teams', async () => {
    const createProject = vi.fn(() => of('id'));
    const { type, submit, ownerTeamField } = await setup(createProject, { teams: [INFRA, WEB] });
    expect(ownerTeamField()?.textContent).toContain('No team');
    await type('#project-display-name', 'Acme');
    await submit();
    expect(createProject).toHaveBeenCalledWith(expect.objectContaining({ owner_team: null }));
  });

  it('preselects the team of the team page, even without membership', async () => {
    const createProject = vi.fn(() => of('id'));
    const { type, submit, ownerTeamField } = await setup(createProject, { teams: [WEB], preselected: { name: 'infra', display_name: 'Infra' } });
    expect(ownerTeamField()?.textContent).toContain('Infra');
    await type('#project-display-name', 'Acme');
    await submit();
    expect(createProject).toHaveBeenCalledWith(expect.objectContaining({ owner_team: 'infra' }));
  });

  it('sends no owner team after picking No team', async () => {
    const createProject = vi.fn(() => of('id'));
    const { type, submit, pickOwnerTeam } = await setup(createProject, { teams: [INFRA] });
    await pickOwnerTeam('No team');
    await type('#project-display-name', 'Acme');
    await submit();
    expect(createProject).toHaveBeenCalledWith(expect.objectContaining({ owner_team: null }));
  });
});
