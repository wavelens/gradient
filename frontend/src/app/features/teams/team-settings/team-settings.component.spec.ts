/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, Router, convertToParamMap, provideRouter } from '@angular/router';
import { EMPTY, Subject, of, throwError } from 'rxjs';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
import { TeamSettingsComponent } from './team-settings.component';
import { TeamsService } from '@core/services/teams.service';
import { AuthService } from '@core/services/auth.service';
import { NotifyService } from '@core/services/notify.service';
import { ApiError } from '@core/services/api.service';
import { TeamResponse } from '@core/models';

const team: TeamResponse = {
  id: 't1',
  name: 'platform',
  display_name: 'Platform',
  managed: false,
  role: 'admin',
  oidc_group: null,
  scim_group: null,
  new_project_users: false,
  new_project_workers: false,
  new_project_role: null,
};

function setup(superuser: boolean, managed = false) {
  const update = vi.fn().mockReturnValue(of('Team updated'));
  const resolved: TeamResponse = { ...team, managed, oidc_group: managed ? 'ci-admins' : null };
  const remove = vi.fn().mockReturnValue(of('Team deleted'));
  const notify = { success: vi.fn(), failure: vi.fn() };
  const confirm = vi.fn((request: ConfirmRequest) => request.action());
  TestBed.configureTestingModule({
    imports: [TeamSettingsComponent],
    providers: [
      provideRouter([]),
      {
        provide: ActivatedRoute,
        useValue: {
          snapshot: { paramMap: convertToParamMap({ team: 'platform' }), data: { teamAccess: { team: resolved } } },
          data: of({ teamAccess: { team: resolved } }),
        },
      },
      { provide: TeamsService, useValue: { get: () => of(resolved), update, remove } },
      { provide: AuthService, useValue: { user: signal({ superuser }) } },
      { provide: NotifyService, useValue: notify },
      { provide: ConfirmService, useValue: { confirm } },
    ],
  });
  const fixture = TestBed.createComponent(TeamSettingsComponent);
  fixture.detectChanges();
  return { fixture, update, remove, notify, confirm };
}

function buttonByText(root: HTMLElement, label: string): HTMLButtonElement {
  return Array.from(root.querySelectorAll('button')).find((b) => b.querySelector('.gr-button__label')?.textContent?.trim() === label)!;
}

function input(fixture: { nativeElement: HTMLElement }, id: string): HTMLInputElement {
  return fixture.nativeElement.querySelector<HTMLInputElement>(`#${id}`)!;
}

describe('TeamSettingsComponent', () => {
  it('names the team in the breadcrumb by its new display name after a rename', () => {
    const { fixture } = setup(true);
    const component = fixture.componentInstance;
    const crumbs = () =>
      Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('.breadcrumb-link, .breadcrumb-current'))
        .map((crumb) => crumb.textContent?.trim());
    expect(crumbs()).toEqual(['Teams', 'Platform', 'Settings']);

    component.form = { ...component.form, display_name: 'Platform Team' };
    component.save();
    fixture.detectChanges();
    expect(crumbs()).toEqual(['Teams', 'Platform Team', 'Settings']);
  });

  it('shows the settings of a state-managed team read-only', async () => {
    const { fixture } = setup(true, true);
    await fixture.whenStable();
    fixture.detectChanges();
    const element = fixture.nativeElement as HTMLElement;
    expect(element.textContent?.replace(/\s+/g, ' ')).toContain('Settings for this team come from the server configuration. Deactivating and adding extra members still work here.');
    expect(input(fixture, 'team-display-name').value).toBe('Platform');
    expect(input(fixture, 'team-display-name').disabled).toBe(true);
    expect(input(fixture, 'team-oidc-group').value).toBe('ci-admins');
    expect(input(fixture, 'team-oidc-group').disabled).toBe(true);
  });

  it('leaves the new project grants to superusers', () => {
    const { fixture, update } = setup(false);
    expect(input(fixture, 'team-oidc-group')).toBeNull();
    expect(fixture.nativeElement.querySelector('#new-project-users')).toBeNull();

    const component = fixture.componentInstance;
    component.form = { ...component.form, display_name: 'Platform Team', new_project_workers: true };
    component.save();
    expect(update).toHaveBeenCalledWith('platform', { display_name: 'Platform Team' });
  });

  it('lets a superuser grant the team on every new project', () => {
    const { fixture, update } = setup(true);
    expect(input(fixture, 'team-oidc-group').disabled).toBe(false);

    const component = fixture.componentInstance;
    component.form = { ...component.form, new_project_workers: true };
    component.save();
    expect(update).toHaveBeenCalledWith('platform', { new_project_workers: true });
  });

  it('picks a role only while new projects grant the team users, starting at View', async () => {
    const { fixture } = setup(true);
    await fixture.whenStable();
    fixture.detectChanges();
    expect(input(fixture, 'new-project-role').disabled).toBe(true);

    fixture.componentInstance.form.new_project_users = true;
    fixture.componentInstance.setDefaultNewProjectRoleIfNoneChosen(true);
    expect(fixture.componentInstance.form.new_project_role).toBe('View');
  });

  it('reports a failed save as a toast', () => {
    const { fixture, update, notify } = setup(true);
    const failure = new ApiError('Display name taken', 409);
    update.mockReturnValue(throwError(() => failure));
    const component = fixture.componentInstance;
    component.form = { ...component.form, display_name: 'Ops' };
    component.save();
    fixture.detectChanges();
    expect(notify.failure).toHaveBeenCalledWith('Failed to save team', failure);
    expect(component.saving()).toBe(false);
    expect(notify.success).not.toHaveBeenCalled();
  });

  it('sends one PATCH for two clicks on Save Changes', async () => {
    const { fixture, update } = setup(true);
    const pending = new Subject<string>();
    update.mockReturnValue(pending);
    await fixture.whenStable();
    const name = input(fixture, 'team-display-name');
    name.value = 'Ops';
    name.dispatchEvent(new Event('input'));
    fixture.detectChanges();
    const save = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll<HTMLButtonElement>('button')).find(
      (button) => button.textContent?.includes('Save Changes'),
    )!;
    save.click();
    fixture.detectChanges();
    save.click();
    pending.next('Team updated');
    pending.complete();
    fixture.detectChanges();
    save.click();
    expect(update).toHaveBeenCalledTimes(1);
  });

  it('keeps Save Changes disabled while nothing changed', () => {
    const { fixture, update } = setup(true);
    const save = buttonByText(fixture.nativeElement, 'Save Changes');
    expect(save.disabled).toBe(true);
    expect(fixture.nativeElement.textContent).not.toContain('No changes to save.');
    fixture.componentInstance.save();
    expect(update).not.toHaveBeenCalled();
  });

  it('shows an emptied display name under the field and sends nothing, even on Enter', () => {
    const { fixture, update } = setup(true);
    fixture.componentInstance.form.display_name = ' ';
    fixture.componentRef.changeDetectorRef.markForCheck();
    fixture.detectChanges();
    const name = input(fixture, 'team-display-name');
    expect(name.closest('gr-form-field')!.textContent).toContain('Enter a display name.');
    name.dispatchEvent(new KeyboardEvent('keyup', { key: 'Enter', bubbles: true }));
    buttonByText(fixture.nativeElement, 'Save Changes').click();
    expect(update).not.toHaveBeenCalled();
  });

  it('reports a save with one toast and no banner', () => {
    const { fixture, notify } = setup(true);
    const component = fixture.componentInstance;
    component.form.display_name = 'Ops';
    component.save();
    fixture.detectChanges();
    expect(notify.success).toHaveBeenCalledWith('Team settings saved');
    expect(fixture.nativeElement.querySelector('gr-message-banner')).toBeNull();
  });

  it('asks before leaving only with unsaved edits', () => {
    const { fixture, confirm } = setup(true);
    confirm.mockImplementation(() => EMPTY);
    expect(fixture.componentInstance.canDeactivate()).toBe(true);
    expect(confirm).not.toHaveBeenCalled();
    fixture.componentInstance.form.oidc_group = 'ci-admins';
    expect(fixture.componentInstance.canDeactivate()).not.toBe(true);
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ target: 'Team settings' }));
  });

  it('asks before deleting, naming the team, then leaves for the team list', () => {
    const { fixture, remove, notify, confirm } = setup(true);
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    buttonByText(fixture.nativeElement, 'Delete Team').click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Delete team', target: 'Platform' }));
    expect(remove).toHaveBeenCalledWith('platform');
    expect(notify.success).toHaveBeenCalledWith('Team deleted');
    expect(String(navigate.mock.calls.at(-1)?.[0])).toBe('/teams');
  });

  it('sends no delete when the confirmation is cancelled', () => {
    const { fixture, remove, confirm } = setup(true);
    confirm.mockImplementation(() => EMPTY);
    buttonByText(fixture.nativeElement, 'Delete Team').click();
    expect(remove).not.toHaveBeenCalled();
  });
});
