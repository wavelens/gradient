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
  const get = vi.fn(() => of<TeamResponse>({ ...team, managed, oidc_group: managed ? 'ci-admins' : null }));
  const remove = vi.fn().mockReturnValue(of('Team deleted'));
  const notify = { success: vi.fn(), failure: vi.fn() };
  const confirm = vi.fn((request: ConfirmRequest) => request.action());
  TestBed.configureTestingModule({
    imports: [TeamSettingsComponent],
    providers: [
      provideRouter([]),
      { provide: ActivatedRoute, useValue: { snapshot: { paramMap: convertToParamMap({ team: 'platform' }) } } },
      { provide: TeamsService, useValue: { get, update, remove } },
      { provide: AuthService, useValue: { user: signal({ superuser }) } },
      { provide: NotifyService, useValue: notify },
      { provide: ConfirmService, useValue: { confirm } },
    ],
  });
  const fixture = TestBed.createComponent(TeamSettingsComponent);
  fixture.detectChanges();
  return { fixture, update, get, remove, notify, confirm };
}

function buttonByText(root: HTMLElement, label: string): HTMLButtonElement {
  return Array.from(root.querySelectorAll('button')).find((b) => b.querySelector('.gr-button__label')?.textContent?.trim() === label)!;
}

function input(fixture: { nativeElement: HTMLElement }, id: string): HTMLInputElement {
  return fixture.nativeElement.querySelector<HTMLInputElement>(`#${id}`)!;
}

describe('TeamSettingsComponent', () => {
  it('names the team in the breadcrumb by its new display name after a rename', () => {
    const { fixture, get } = setup(true);
    const component = fixture.componentInstance;
    const crumbs = () =>
      Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('.breadcrumb-link, .breadcrumb-current'))
        .map((crumb) => crumb.textContent?.trim());
    expect(crumbs()).toEqual(['Teams', 'Platform', 'Settings']);

    get.mockReturnValue(of({ ...team, display_name: 'Platform Team' }));
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
    expect(element.textContent).toContain('managed by the server configuration');
    expect(input(fixture, 'team-display-name').value).toBe('Platform');
    expect(input(fixture, 'team-display-name').disabled).toBe(true);
    expect(input(fixture, 'team-oidc-group').value).toBe('ci-admins');
    expect(input(fixture, 'team-oidc-group').disabled).toBe(true);
  });

  it('leaves the new project grants to superusers', () => {
    const { fixture, update } = setup(false);
    expect(input(fixture, 'team-oidc-group').disabled).toBe(true);

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
    expect(component.saved()).toBe(false);
  });

  it('sends one PATCH for two clicks on Save Changes', async () => {
    const { fixture, update, get } = setup(true);
    const pending = new Subject<string>();
    update.mockReturnValue(pending);
    get.mockReturnValue(new Subject<TeamResponse>());
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

  it('keeps Save Changes disabled and says why while nothing changed', () => {
    const { fixture, update } = setup(true);
    const save = buttonByText(fixture.nativeElement, 'Save Changes');
    expect(save.disabled).toBe(true);
    expect(fixture.nativeElement.textContent).toContain('No changes to save.');
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

  it('drops the saved banner as soon as the form changes again', () => {
    const { fixture } = setup(true);
    const component = fixture.componentInstance;
    component.form.display_name = 'Ops';
    component.save();
    fixture.detectChanges();
    expect(fixture.nativeElement.textContent).toContain('Changes saved.');
    component.form.display_name = 'Ops Team';
    fixture.componentRef.changeDetectorRef.markForCheck();
    fixture.detectChanges();
    expect(fixture.nativeElement.textContent).not.toContain('Changes saved.');
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
