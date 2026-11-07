/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { Subject, of, throwError } from 'rxjs';
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
  TestBed.configureTestingModule({
    imports: [TeamSettingsComponent],
    providers: [
      provideRouter([]),
      { provide: ActivatedRoute, useValue: { snapshot: { paramMap: convertToParamMap({ team: 'platform' }) } } },
      { provide: TeamsService, useValue: { get, update, remove } },
      { provide: AuthService, useValue: { user: signal({ superuser }) } },
      { provide: NotifyService, useValue: notify },
    ],
  });
  const fixture = TestBed.createComponent(TeamSettingsComponent);
  fixture.detectChanges();
  return { fixture, update, get, remove, notify };
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

  it('keeps the delete dialog open while the deletion runs', () => {
    const { fixture, remove } = setup(true);
    remove.mockReturnValue(new Subject());
    const component = fixture.componentInstance;
    component.openDelete();
    fixture.detectChanges();
    component.remove();
    fixture.detectChanges();
    const dialog = document.querySelector('.gr-dialog')!;
    dialog.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', keyCode: 27, bubbles: true }));
    (document.querySelector('.gr-dialog-backdrop') as HTMLElement).click();
    fixture.detectChanges();
    expect(component.showDelete()).toBe(true);
    expect(dialog.getAttribute('aria-busy')).toBe('true');
  });

  it('keeps a failed deletion inside the open dialog', () => {
    const { fixture, remove } = setup(true);
    remove.mockReturnValue(throwError(() => new ApiError('Team still owns projects', 409)));
    const component = fixture.componentInstance;
    component.openDelete();
    component.remove();
    fixture.detectChanges();
    expect(component.showDelete()).toBe(true);
    expect(document.querySelector('.gr-dialog')?.textContent).toContain('Team still owns projects');
    expect((fixture.nativeElement as HTMLElement).textContent).not.toContain('Team still owns projects');
  });
});
