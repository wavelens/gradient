/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { Observable, of, throwError } from 'rxjs';
import { TaskSettingsComponent } from './task-settings.component';
import { TasksService } from '@core/services/tasks.service';
import { ProjectsService } from '@core/services/projects.service';
import { AccessState } from '@core/models/access.model';

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
    repository: '',
    wildcard: '',
    active: true,
    force_evaluation: false,
    keep_evaluations: 30,
    concurrency: 'soft_abort' as const,
    sign_cache: true,
    managed: c.managed,
    can_edit: c.canEdit,
    can_trigger: c.canTrigger ?? c.canEdit,
  };
}

function activatedRouteStub(c: AccessCase): ActivatedRoute {
  return {
    snapshot: { paramMap: convertToParamMap({ project: 'acme', task: 'demo' }) },
    data: of({}),
    parent: { data: of({ taskAccess: { task: taskFor(c), access: asAccess(c) } }) },
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
  checkRepository: () => Observable<string> = () => of('abc1234def5678'),
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
        useValue: { getTaskInfo: () => of(taskFor(c)), checkRepository },
      },
      {
        provide: ProjectsService,
        useValue: { getProject: () => of({ id: 'o', display_name: 'Acme' }) },
      },
    ],
  });
  const fixture = TestBed.createComponent(TaskSettingsComponent);
  fixture.detectChanges();
  return fixture;
}

describe('TaskSettingsComponent - access gating', () => {
  it('hides Save Changes when access is read-only', () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    expect(findByText(fixture.nativeElement, 'save changes')).toBeNull();
  });

  it('shows but disables Save Changes when task is state-managed', () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    const btn = findByText(fixture.nativeElement, 'save changes') as HTMLButtonElement | null;
    expect(btn).not.toBeNull();
    expect(btn!.disabled).toBe(true);
  });

  it('hides Delete Task under read-only access', () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    expect(findByText(fixture.nativeElement, 'delete task')).toBeNull();
  });

  it('shows but disables Delete Task when managed', () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    const btn = findByText(fixture.nativeElement, 'delete task') as HTMLButtonElement | null;
    expect(btn).not.toBeNull();
    expect(btn!.disabled).toBe(true);
  });
});

describe('TaskSettingsComponent - repository connection test', () => {
  const editor = { managed: false, canEdit: true };
  const status = (fixture: ComponentFixture<TaskSettingsComponent>) =>
    (fixture.nativeElement as HTMLElement).querySelector('.repo-check')?.textContent?.trim() ?? '';

  it('reports the head it reached', () => {
    const fixture = setup(editor);
    (findByText(fixture.nativeElement, 'test connection') as HTMLButtonElement).click();
    fixture.detectChanges();
    expect(status(fixture)).toContain('abc1234');
  });

  it('shows why the repository could not be reached', () => {
    const fixture = setup(editor, () => throwError(() => new Error('Permission denied (publickey)')));
    (findByText(fixture.nativeElement, 'test connection') as HTMLButtonElement).click();
    fixture.detectChanges();
    expect(status(fixture)).toContain('Permission denied (publickey)');
  });

  /// The server checks the saved URL, so an edited one would test the old remote.
  it('asks to save an edited URL before testing it', async () => {
    const fixture = setup(editor);
    await fixture.whenStable();
    const input = (fixture.nativeElement as HTMLElement).querySelector('#proj-repo') as HTMLInputElement;
    input.value = 'git@example.org:other.git';
    input.dispatchEvent(new Event('input'));
    fixture.detectChanges();
    const btn = findByText(fixture.nativeElement, 'test connection') as HTMLButtonElement;
    expect(btn.disabled).toBe(true);
  });

  it('is not offered to a viewer', () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    expect(findByText(fixture.nativeElement, 'test connection')).toBeNull();
  });
});
