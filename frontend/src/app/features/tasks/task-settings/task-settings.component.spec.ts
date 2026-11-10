/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, Router, convertToParamMap, provideRouter } from '@angular/router';
import { taskAccessData } from '@core/resolvers/access-route.testing';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { EMPTY, NEVER, Observable, of, throwError } from 'rxjs';
import { vi } from 'vitest';
import { TaskSettingsComponent } from './task-settings.component';
import { TasksService } from '@core/services/tasks.service';
import { ProjectsService } from '@core/services/projects.service';
import { AccessState } from '@core/models/access.model';
import { ConfigService } from '@core/services/config.service';
import { NotifyService } from '@core/services/notify.service';
import { ApiError } from '@core/services/api.service';
import { ConfirmService, type ConfirmRequest } from '@shared/confirm';

type AccessCase = { managed: boolean; canEdit: boolean; canTrigger?: boolean };

function asAccess(c: AccessCase): AccessState {
  return { managed: c.managed, canEdit: c.canEdit, canTrigger: c.canTrigger ?? c.canEdit };
}

function taskFor(c: AccessCase) {
  return {
    id: 'p',
    project: 'acme',
    name: 'demo',
    display_name: 'Demo',
    description: '',
    repository: 'git@example.org:demo.git',
    wildcard: 'packages.x86_64-linux.**',
    active: true,
    force_evaluation: false,
    keep_evaluations: 30,
    concurrency: 'soft_abort' as const,
    sign_cache: true,
    wait_for_workers: false,
    retry_failed_builds: true,
    multi_node_evaluation: false,
    exec_enabled: false,
    only_evaluate: false,
    managed: c.managed,
    can_edit: c.canEdit,
    can_trigger: c.canTrigger ?? c.canEdit,
  };
}

function activatedRouteStub(c: AccessCase): ActivatedRoute {
  return {
    snapshot: { paramMap: convertToParamMap({ project: 'acme', task: 'demo' }) },
    data: of({}),
    parent: { data: of(taskAccessData(asAccess(c), taskFor(c))) },
  } as unknown as ActivatedRoute;
}

function findByText(root: HTMLElement, text: string): HTMLElement | null {
  const target = text.toLowerCase();
  return (Array.from(root.querySelectorAll('button')) as HTMLElement[]).find(
    (el) => (el.textContent ?? '').trim().toLowerCase().includes(target),
  ) ?? null;
}

function setup(
  c: AccessCase,
  checkRepository: (...args: unknown[]) => Observable<string> = () => of('abc1234def5678'),
  updateTask: (...args: unknown[]) => Observable<string> = () => of('ok'),
  execEnabled = false,
): ComponentFixture<TaskSettingsComponent> {
  TestBed.configureTestingModule({
    imports: [TaskSettingsComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: activatedRouteStub(c) },
      {
        provide: TasksService,
        useValue: { getTaskInfo: () => of(taskFor(c)), checkRepository, updateTask },
      },
      {
        provide: ProjectsService,
        useValue: {
          getProject: () => of({ id: 'o', display_name: 'Acme' }),
          getSSHKey: () => of('ssh-ed25519 AAAA acme@gradient'),
        },
      },
    ],
  });
  TestBed.inject(ConfigService).execEnabled = execEnabled;
  const fixture = TestBed.createComponent(TaskSettingsComponent);
  fixture.detectChanges();
  return fixture;
}

describe('TaskSettingsComponent - access gating', () => {
  it('hides Save Changes when access is read-only', () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    expect(findByText(fixture.nativeElement, 'save changes')).toBeNull();
  });

  it('hides Save Changes when task is state-managed', () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    expect(fixture.nativeElement.querySelector('gr-message-banner')?.textContent).toContain('server configuration');
    expect(findByText(fixture.nativeElement, 'save changes')).toBeNull();
  });

  it('hides Delete Task under read-only access', () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    expect(findByText(fixture.nativeElement, 'delete task')).toBeNull();
  });

  it('hides the Danger Zone when managed', () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    expect(findByText(fixture.nativeElement, 'delete task')).toBeNull();
    expect(findByText(fixture.nativeElement, 'transfer ownership')).toBeNull();
  });

  it('explains why the name is locked and points to the display name', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    const field = (fixture.nativeElement as HTMLElement).querySelector('#task-name')?.closest('gr-form-field');
    expect(field?.textContent).toContain('Edit the Display Name instead.');
  });

  it('describes the active status by what triggers do', () => {
    const fixture = setup({ managed: false, canEdit: true });
    expect(fixture.nativeElement.textContent).toContain('Triggers start an evaluation.');
  });
});

describe('TaskSettingsComponent - repository', () => {
  it('links to the Repository page for the URL, key, events and status', () => {
    const fixture = setup({ managed: false, canEdit: true });
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('#task-repo')).toBeNull();
    const link = Array.from(root.querySelectorAll('a')).find((a) => a.getAttribute('href') === '/project/acme/task/demo/repository');
    expect(link).toBeDefined();
  });
});

describe('TaskSettingsComponent - validation and unsaved edits', () => {
  const editor = { managed: false, canEdit: true };

  async function typeInto(fixture: ComponentFixture<TaskSettingsComponent>, selector: string, value: string) {
    await fixture.whenStable();
    const input = (fixture.nativeElement as HTMLElement).querySelector(selector) as HTMLInputElement;
    input.value = value;
    input.dispatchEvent(new Event('input'));
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
  }

  const save = (fixture: ComponentFixture<TaskSettingsComponent>) =>
    findByText(fixture.nativeElement, 'save changes') as HTMLButtonElement;
  const fieldError = (fixture: ComponentFixture<TaskSettingsComponent>) =>
    (fixture.nativeElement as HTMLElement).querySelector('.field-error')?.textContent?.trim() ?? '';

  it('disables Save without a hint while nothing changed', () => {
    const fixture = setup(editor);
    expect(save(fixture).disabled).toBe(true);
    expect((fixture.nativeElement as HTMLElement).querySelector('.save-hint')).toBeNull();
  });

  it.each(['0', '-5', '1.5'])('refuses %s evaluations to keep without sending it', async (value) => {
    const updateTask = vi.fn(() => of('ok'));
    const fixture = setup(editor, undefined, updateTask);
    await typeInto(fixture, '#task-keep', value);
    expect(fieldError(fixture)).toContain('1 or higher');
    expect(save(fixture).disabled).toBe(true);
    fixture.componentInstance.saveSettings();
    expect(updateTask).not.toHaveBeenCalled();
  });

  it('marks a number above the server limit on the field without sending it', async () => {
    const updateTask = vi.fn(() => of('ok'));
    const fixture = setup(editor, undefined, updateTask);
    TestBed.inject(ConfigService).maxKeepEvaluationsPerTask = 30;
    await typeInto(fixture, '#task-keep', '100000');
    expect(fieldError(fixture)).toBe('This server can keep at most 30 evaluations per task.');
    fixture.componentInstance.saveSettings();
    expect(updateTask).not.toHaveBeenCalled();
  });

  it('refuses an empty number of evaluations instead of saving null', async () => {
    const updateTask = vi.fn(() => of('ok'));
    const fixture = setup(editor, undefined, updateTask);
    await typeInto(fixture, '#task-keep', '');
    expect(fieldError(fixture)).toContain('number of evaluations');
    fixture.componentInstance.saveSettings();
    expect(updateTask).not.toHaveBeenCalled();
  });

  it('marks an invalid wildcard on the field and focuses it instead of saving', async () => {
    const updateTask = vi.fn(() => of('ok'));
    const fixture = setup(editor, undefined, updateTask);
    Element.prototype.scrollIntoView = vi.fn();
    await typeInto(fixture, '#task-wildcard', 'packages.#');
    expect(fieldError(fixture)).toContain('`#`');
    fixture.componentInstance.saveSettings();
    fixture.detectChanges();
    await fixture.whenStable();
    expect(updateTask).not.toHaveBeenCalled();
    expect(document.activeElement?.id).toBe('task-wildcard');
  });

  it('refuses an empty display name', async () => {
    const updateTask = vi.fn(() => of('ok'));
    const fixture = setup(editor, undefined, updateTask);
    await typeInto(fixture, '#task-display', '  ');
    expect(fieldError(fixture)).toContain('display name');
    fixture.componentInstance.saveSettings();
    expect(updateTask).not.toHaveBeenCalled();
  });

  it('sends only the changed fields when Enter is pressed in a field', async () => {
    const updateTask = vi.fn(() => of('ok'));
    const fixture = setup(editor, undefined, updateTask);
    await typeInto(fixture, '#task-display', 'Renamed');
    const input = (fixture.nativeElement as HTMLElement).querySelector('#task-display') as HTMLInputElement;
    input.dispatchEvent(new KeyboardEvent('keyup', { key: 'Enter' }));
    expect(updateTask).toHaveBeenCalledWith('acme', 'demo', { display_name: 'Renamed' });
  });

  it('shows a failed save beside Save Changes and scrolls it into view', async () => {
    const scroll = vi.fn();
    Element.prototype.scrollIntoView = scroll;
    const updateTask = vi.fn(() => throwError(() => new ApiError('branch does not exist', 400)));
    const fixture = setup(editor, undefined, updateTask);
    await typeInto(fixture, '#task-display', 'Renamed');
    fixture.componentInstance.saveSettings();
    fixture.detectChanges();
    await fixture.whenStable();
    const banner = (fixture.nativeElement as HTMLElement).querySelector('gr-message-banner');

    expect(banner?.textContent).toContain('branch does not exist');
    expect(banner?.parentElement?.querySelector('.gr-form-actions button')?.textContent).toContain('Save Changes');
    expect(scroll).toHaveBeenCalledWith({ block: 'nearest' });
  });

  it('reports a save with one toast and no banner', async () => {
    const fixture = setup(editor);
    const success = vi.spyOn(TestBed.inject(NotifyService), 'success');
    await typeInto(fixture, '#task-display', 'Renamed');
    fixture.componentInstance.saveSettings();
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    expect(success).toHaveBeenCalledWith('Task settings saved');
    expect(root.textContent).not.toContain('Changes saved.');
  });

  it('asks before leaving only with unsaved edits', async () => {
    const confirm = vi.fn(() => EMPTY);
    TestBed.overrideProvider(ConfirmService, { useValue: { confirm } });
    const fixture = setup(editor);
    expect(fixture.componentInstance.canDeactivate()).toBe(true);
    expect(confirm).not.toHaveBeenCalled();

    await typeInto(fixture, '#task-display', 'Renamed');
    expect(fixture.componentInstance.canDeactivate()).not.toBe(true);
    expect(confirm).toHaveBeenCalledOnce();
  });
});

describe('TaskSettingsComponent - write failures', () => {
  const admin = { managed: false, canEdit: true, canTrigger: true };
  const refused = new ApiError('You need admin rights on this project', 403);

  function tasks(): Record<string, unknown> {
    return TestBed.inject(TasksService) as unknown as Record<string, unknown>;
  }

  it('reports a failed deactivate with the server reason and frees the button', () => {
    const fixture = setup(admin);
    tasks()['deactivateTask'] = () => throwError(() => refused);
    const failure = vi.spyOn(TestBed.inject(NotifyService), 'failure');
    fixture.componentInstance.toggleActive();
    expect(failure).toHaveBeenCalledWith('Failed to deactivate task', refused);
    expect(fixture.componentInstance.toggling()).toBe(false);
  });

  it('reports a successful deactivate', () => {
    const fixture = setup(admin);
    tasks()['deactivateTask'] = () => of('ok');
    const success = vi.spyOn(TestBed.inject(NotifyService), 'success');
    fixture.componentInstance.toggleActive();
    expect(success).toHaveBeenCalledWith('Task deactivated');
  });

  it('asks first, naming the task, then deletes it and leaves the page', () => {
    const confirm = vi.fn((request: ConfirmRequest): Observable<unknown> => request.action());
    TestBed.overrideProvider(ConfirmService, { useValue: { confirm } });
    const fixture = setup(admin);
    const deleteTask = vi.fn(() => of('ok'));
    tasks()['deleteTask'] = deleteTask;
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    findByText(fixture.nativeElement, 'delete task')!.click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Delete task', target: 'Demo' }));
    expect(deleteTask).toHaveBeenCalledWith('acme', 'demo');
    expect(String(navigate.mock.calls.at(-1)?.[0])).toBe('/project/acme');
  });

  it('sends nothing when the delete is cancelled', () => {
    TestBed.overrideProvider(ConfirmService, { useValue: { confirm: vi.fn(() => EMPTY) } });
    const fixture = setup(admin);
    const deleteTask = vi.fn(() => of('ok'));
    tasks()['deleteTask'] = deleteTask;
    findByText(fixture.nativeElement, 'delete task')!.click();
    expect(deleteTask).not.toHaveBeenCalled();
  });

  it('sends one update when Save Changes is clicked twice', async () => {
    const updateTask = vi.fn(() => NEVER);
    const fixture = setup(admin, undefined, updateTask);
    await fixture.whenStable();
    const display = (fixture.nativeElement as HTMLElement).querySelector('#task-display') as HTMLInputElement;
    display.value = 'Renamed';
    display.dispatchEvent(new Event('input'));
    fixture.detectChanges();
    const save = findByText(fixture.nativeElement, 'save changes') as HTMLButtonElement;
    save.click();
    fixture.detectChanges();
    save.click();
    expect(updateTask).toHaveBeenCalledOnce();
  });

  it('opens the moved task at once, so no delayed redirect can overtake a later click', () => {
    vi.useFakeTimers();
    try {
      const fixture = setup(admin);
      tasks()['transferOwnership'] = () => of('ok');
      const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
      const success = vi.spyOn(TestBed.inject(NotifyService), 'success');
      const component = fixture.componentInstance;
      component.showTransferDialog.set(true);
      component.transferProjectName = 'other';
      component.transferOwnership();

      expect(component.showTransferDialog()).toBe(false);
      expect(success).toHaveBeenCalledWith('Task transferred to other');
      expect(navigate).toHaveBeenCalledOnce();
      expect(String(navigate.mock.calls[0][0])).toBe('/project/other/task/demo');
      vi.advanceTimersByTime(5000);
      expect(navigate).toHaveBeenCalledOnce();
    } finally {
      vi.useRealTimers();
    }
  });

  it('offers the other unmanaged projects by display name as transfer targets', () => {
    const fixture = setup(admin);
    const project = (name: string, display_name: string, managed = false) => ({ name, display_name, managed });
    (TestBed.inject(ProjectsService) as unknown as Record<string, unknown>)['getProjects'] = () =>
      of({ items: [project('acme', 'Acme'), project('other', 'Other Team'), project('nix', 'Nix Managed', true)] });
    const component = fixture.componentInstance;
    component.openTransferOwnershipDialogAndLoadTargetProjects();
    expect(component.showTransferDialog()).toBe(true);
    expect(component.transferTargetProjectOptions()).toEqual([{ label: 'Other Team', value: 'other' }]);
  });

  it('shows a refused transfer inside the open dialog, not on the page', () => {
    const fixture = setup(admin);
    tasks()['transferOwnership'] = () => throwError(() => new ApiError('Project not found', 404));
    const component = fixture.componentInstance;
    component.showTransferDialog.set(true);
    component.transferProjectName = 'nowhere';
    fixture.detectChanges();
    component.transferOwnership();
    fixture.detectChanges();

    expect(component.showTransferDialog()).toBe(true);
    expect(document.body.querySelector('.gr-dialog gr-message-banner')?.textContent).toContain('Project not found');
    expect((fixture.nativeElement as HTMLElement).textContent).not.toContain('Project not found');
  });
});
