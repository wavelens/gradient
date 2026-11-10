/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, Router, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { EMPTY, Subject, of, throwError } from 'rxjs';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
import { ProjectSettingsComponent } from './project-settings.component';
import { ProjectsService } from '@core/services/projects.service';
import { EVERY_PERMISSION, projectRouteStub } from '@core/resolvers/access-route.testing';
import { AccessState } from '@core/models/access.model';
import type { Permission } from '@core/models';
import { ProjectResponse } from '@core/models';
import { ApiError } from '@core/services/api.service';
import { NotifyService } from '@core/services/notify.service';

const notify = { success: vi.fn(), failure: vi.fn() };
const confirm = vi.fn();

function projectFor(access: AccessState): ProjectResponse {
  return {
    id: 'o',
    name: 'acme',
    display_name: 'Acme',
    description: '',
    public: false,
    hide_build_requests: false,
    allow_exec_in_build_requests: false,
    managed: access.managed,
    created_by: 'u1',
    created_at: '2026-01-01T00:00:00',
    github_app_available: false,
    role: access.canEdit ? 'Admin' : 'View',
    permissions: access.canEdit ? EVERY_PERMISSION : ['viewProject'],
  };
}

function findByText(root: HTMLElement, text: string): HTMLElement | null {
  const target = text.toLowerCase();
  return (Array.from(root.querySelectorAll('button')) as HTMLElement[]).find(
    (el) => (el.textContent ?? '').trim().toLowerCase().includes(target),
  ) ?? null;
}

function setup(access: AccessState, permissions?: Permission[], fields: Partial<ProjectResponse> = {}): ComponentFixture<ProjectSettingsComponent> {
  notify.success.mockClear();
  notify.failure.mockClear();
  confirm.mockReset().mockImplementation((request: ConfirmRequest) => request.action());
  TestBed.configureTestingModule({
    imports: [ProjectSettingsComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useFactory: () => projectRouteStub('acme', access, { ...(permissions && { permissions }), ...fields }) },
      {
        provide: ProjectsService,
        useValue: {
          getProject: () => of(projectFor(access)),
          getSSHKey: () => of('ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEd25519 acme'),
        },
      },
      { provide: NotifyService, useValue: notify },
      { provide: ConfirmService, useValue: { confirm } },
    ],
  });
  const fixture = TestBed.createComponent(ProjectSettingsComponent);
  fixture.detectChanges();
  return fixture;
}

async function settled(fixture: ComponentFixture<ProjectSettingsComponent>) {
  await fixture.whenStable();
  fixture.detectChanges();
}

async function typeDisplayName(fixture: ComponentFixture<ProjectSettingsComponent>, value: string) {
  await settled(fixture);
  const input = (fixture.nativeElement as HTMLElement).querySelector('#display-name') as HTMLInputElement;
  input.value = value;
  input.dispatchEvent(new Event('input'));
  await settled(fixture);
}

describe('ProjectSettingsComponent - validation and unsaved edits', () => {
  const admin: AccessState = { managed: false, canEdit: true, canTrigger: true };
  const save = (fixture: ComponentFixture<ProjectSettingsComponent>) =>
    findByText(fixture.nativeElement, 'save changes') as HTMLButtonElement;

  function spyOnWrites() {
    const writes = {
      updateProject: vi.fn(() => of('ok')),
      setPublic: vi.fn(() => of('ok')),
      setPrivate: vi.fn(() => of('ok')),
    };
    Object.assign(TestBed.inject(ProjectsService), writes);
    return writes;
  }

  it('says why the locked name cannot change and points to the display name', async () => {
    const fixture = setup(admin);
    await settled(fixture);
    const nameField = (fixture.nativeElement as HTMLElement).querySelector('#project-name')!.closest('gr-form-field')!;
    expect(nameField.querySelector('.hint')?.textContent).toContain('display name');
  });

  it('disables Save without a hint while nothing changed', async () => {
    const fixture = setup(admin);
    await settled(fixture);
    expect(save(fixture).disabled).toBe(true);
    expect((fixture.nativeElement as HTMLElement).querySelector('.save-hint')).toBeNull();
  });

  it('shows an error for an empty display name and sends nothing', async () => {
    const fixture = setup(admin);
    const writes = spyOnWrites();
    await typeDisplayName(fixture, ' ');
    expect((fixture.nativeElement as HTMLElement).querySelector('.field-error')?.textContent).toContain('display name');
    expect(save(fixture).disabled).toBe(true);
    fixture.componentInstance.saveSettings();
    expect(writes.updateProject).not.toHaveBeenCalled();
  });

  it('sends only the changed fields and skips an unchanged visibility on Enter', async () => {
    const fixture = setup(admin);
    const writes = spyOnWrites();
    await typeDisplayName(fixture, 'Renamed');
    (fixture.nativeElement as HTMLElement)
      .querySelector('#display-name')!
      .dispatchEvent(new KeyboardEvent('keyup', { key: 'Enter' }));
    expect(writes.updateProject).toHaveBeenCalledWith('acme', { display_name: 'Renamed' });
    expect(writes.setPublic).not.toHaveBeenCalled();
    expect(writes.setPrivate).not.toHaveBeenCalled();
  });

  it('changes only the visibility when only the visibility changed', async () => {
    const fixture = setup(admin);
    const writes = spyOnWrites();
    await settled(fixture);
    fixture.componentInstance.formData = { ...fixture.componentInstance.formData, public: true };
    fixture.componentInstance.saveSettings();
    expect(writes.updateProject).not.toHaveBeenCalled();
    expect(writes.setPublic).toHaveBeenCalledWith('acme');
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Make project public', target: 'Acme' }));
  });

  it('sends nothing when making the project public is cancelled', async () => {
    const fixture = setup(admin);
    const writes = spyOnWrites();
    await settled(fixture);
    confirm.mockImplementation(() => EMPTY);
    fixture.componentInstance.formData = { ...fixture.componentInstance.formData, display_name: 'Renamed', public: true };
    fixture.componentInstance.saveSettings();
    expect(writes.updateProject).not.toHaveBeenCalled();
    expect(writes.setPublic).not.toHaveBeenCalled();
    expect(fixture.componentInstance.saving()).toBe(false);
    expect(notify.success).not.toHaveBeenCalled();
  });

  it('reports a save with one toast and no hint beside the disabled Save', async () => {
    const fixture = setup(admin);
    spyOnWrites();
    await typeDisplayName(fixture, 'Renamed');
    fixture.componentInstance.saveSettings();
    await settled(fixture);
    const root = fixture.nativeElement as HTMLElement;
    expect(notify.success).toHaveBeenCalledWith('Project settings saved');
    expect(root.textContent).not.toContain('Changes saved.');
    expect(root.textContent).not.toContain('no changes');
  });

  it('asks before leaving only with unsaved edits', async () => {
    const fixture = setup(admin);
    confirm.mockImplementation(() => EMPTY);
    await settled(fixture);
    expect(fixture.componentInstance.canDeactivate()).toBe(true);
    expect(confirm).not.toHaveBeenCalled();

    await typeDisplayName(fixture, 'Renamed');
    expect(fixture.componentInstance.canDeactivate()).not.toBe(true);
    expect(confirm).toHaveBeenCalledOnce();
  });
});

describe('ProjectSettingsComponent - access gating', () => {
  it('hides Save / Delete under read-only access and says so once', async () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    await settled(fixture);
    expect(findByText(fixture.nativeElement, 'save changes')).toBeNull();
    expect(findByText(fixture.nativeElement, 'delete project')).toBeNull();
    expect(fixture.nativeElement.querySelector('gr-message-banner')?.textContent).toContain('Only the admins of this project can change these settings.');
  });

  it('links a visitor without a role to no member-only page', async () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false }, [], { role: null });
    await settled(fixture);
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).not.toContain('Members & Roles');
    expect(text).not.toContain('Workers');
    expect(text).not.toContain('SSH Key');
  });

  it('hides Save and disables Delete under state-managed access', async () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    await settled(fixture);
    const del = findByText(fixture.nativeElement, 'delete project') as HTMLButtonElement | null;
    expect(findByText(fixture.nativeElement, 'save changes')).toBeNull();
    expect(del).not.toBeNull();
    expect(del!.getAttribute('aria-disabled')).toBe('true');
  });
});

describe('ProjectSettingsComponent - permissions', () => {
  const writable: AccessState = { managed: false, canEdit: true, canTrigger: true };

  it('offers no SSH key regeneration without the manage SSH key permission', async () => {
    const fixture = setup(writable, ['viewProject', 'manageProjectSettings', 'deleteProject']);
    await settled(fixture);
    expect(findByText(fixture.nativeElement, 'regenerate key')).toBeNull();
    expect(findByText(fixture.nativeElement, 'delete project')).not.toBeNull();
    expect(findByText(fixture.nativeElement, 'save changes')).not.toBeNull();
  });

  it('offers SSH key regeneration but no project deletion to a role with only the SSH key permission', async () => {
    const fixture = setup(writable, ['viewProject', 'manageSshKey']);
    await settled(fixture);
    expect(findByText(fixture.nativeElement, 'regenerate key')).not.toBeNull();
    expect(findByText(fixture.nativeElement, 'delete project')).toBeNull();
  });

  it('links only to the settings pages the role can open', async () => {
    const fixture = setup(writable, ['viewProject', 'manageMembers', 'manageWebhooks']);
    await settled(fixture);
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('Members & Roles');
    expect(text).toContain('Event notifications');
    expect(text).not.toContain('Integrations');
  });
});

describe('ProjectSettingsComponent - SSH key load errors', () => {
  it('shows a failed key load as an error instead of offering to generate a key', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    await settled(fixture);
    const service = TestBed.inject(ProjectsService) as unknown as { getSSHKey: () => unknown };
    service.getSSHKey = () => throwError(() => new Error('database is down'));
    fixture.componentInstance.loadSSHKey();
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.textContent).toContain('Failed to load SSH key');
    expect(root.textContent).not.toContain('No SSH key generated yet.');

    service.getSSHKey = () => of('ssh-ed25519 AAAA acme');
    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')!.click();
    fixture.detectChanges();
    expect(root.textContent).not.toContain('Failed to load SSH key');
  });
});

describe('ProjectSettingsComponent - project load errors', () => {
  it('shows a failed project load with Try Again instead of a blank page', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    await settled(fixture);
    const service = TestBed.inject(ProjectsService) as unknown as { getProject: () => unknown };
    service.getProject = () => throwError(() => new Error('database is down'));
    fixture.componentInstance.loadProject();
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.textContent).toContain('Failed to load project settings');
    expect(root.textContent).toContain('database is down');
  });
});

describe('ProjectSettingsComponent - write failures', () => {
  it('reports a refused SSH key generation instead of failing silently', async () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    await settled(fixture);
    const denied = new ApiError('No permission for this action.', 403);
    const service = TestBed.inject(ProjectsService) as unknown as { generateSSHKey: () => unknown };
    service.generateSSHKey = () => throwError(() => denied);
    fixture.componentInstance.generateSSHKey();
    expect(notify.failure).toHaveBeenCalledWith('SSH key generation failed', denied);
    expect(fixture.componentInstance.generatingSSH()).toBe(false);
  });

  it('sends one PATCH when Save Changes is clicked twice', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    await settled(fixture);
    const updateProject = vi.fn(() => new Subject<void>());
    Object.assign(TestBed.inject(ProjectsService), { updateProject, setPrivate: () => of(undefined) });
    await typeDisplayName(fixture, 'Renamed');
    const save = findByText(fixture.nativeElement, 'save changes')!;
    save.click();
    fixture.detectChanges();
    save.click();
    expect(updateProject).toHaveBeenCalledTimes(1);
  });

});

describe('ProjectSettingsComponent - confirmed actions', () => {
  const admin: AccessState = { managed: false, canEdit: true, canTrigger: true };

  it('asks before deleting, naming the project, then leaves for the project list', async () => {
    const fixture = setup(admin);
    await settled(fixture);
    const deleteProject = vi.fn(() => of(undefined));
    Object.assign(TestBed.inject(ProjectsService), { deleteProject });
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    findByText(fixture.nativeElement, 'delete project')!.click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Delete project', target: projectFor(admin).display_name }));
    expect(deleteProject).toHaveBeenCalledWith('acme');
    expect(String(navigate.mock.calls.at(-1)?.[0])).toBe('/projects');
  });

  it('sends no delete when the confirmation is cancelled', async () => {
    const fixture = setup(admin);
    await settled(fixture);
    confirm.mockImplementation(() => EMPTY);
    const deleteProject = vi.fn(() => of(undefined));
    Object.assign(TestBed.inject(ProjectsService), { deleteProject });
    findByText(fixture.nativeElement, 'delete project')!.click();
    expect(deleteProject).not.toHaveBeenCalled();
  });

  it('asks before regenerating the SSH key, then shows the new key', async () => {
    const fixture = setup(admin);
    await settled(fixture);
    const generateSSHKey = vi.fn(() => of('ssh-ed25519 NEWKEY acme'));
    Object.assign(TestBed.inject(ProjectsService), { generateSSHKey });
    findByText(fixture.nativeElement, 'regenerate key')!.click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Regenerate SSH key', consequence: expect.stringContaining('authorized_keys') }));
    expect(fixture.componentInstance.sshKey()).toBe('ssh-ed25519 NEWKEY acme');
    expect(notify.success).toHaveBeenCalledWith('SSH key generated');
  });

  it('keeps the old SSH key when regenerating is cancelled', async () => {
    const fixture = setup(admin);
    await settled(fixture);
    confirm.mockImplementation(() => EMPTY);
    const generateSSHKey = vi.fn(() => of('ssh-ed25519 NEWKEY acme'));
    Object.assign(TestBed.inject(ProjectsService), { generateSSHKey });
    findByText(fixture.nativeElement, 'regenerate key')!.click();
    expect(generateSSHKey).not.toHaveBeenCalled();
  });
});
