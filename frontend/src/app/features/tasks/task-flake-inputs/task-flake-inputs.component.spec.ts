/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { of, throwError } from 'rxjs';
import { vi } from 'vitest';
import { TaskFlakeInputsComponent } from './task-flake-inputs.component';
import { NotifyService } from '@core/services/notify.service';
import { ApiError } from '@core/services/api.service';
import { FlakeInputOverridesService } from '@core/services/flake-input-overrides.service';
import { ProjectsService } from '@core/services/projects.service';
import { TasksService } from '@core/services/tasks.service';
import { AccessState } from '@core/models/access.model';
import { FlakeInputOverrideOut } from '@core/models';

function activatedRouteStub(access: AccessState): ActivatedRoute {
  return {
    snapshot: { paramMap: convertToParamMap({ project: 'acme', task: 'demo' }) },
    data: of({}),
    parent: { data: of({ taskAccess: { task: {}, access } }) },
  } as unknown as ActivatedRoute;
}

const override: FlakeInputOverrideOut = {
  id: 'o1',
  task: 'p1',
  input_name: 'utils',
  url: 'github:numtide/flake-utils',
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
};

const overrideKeepUrl: FlakeInputOverrideOut = {
  id: 'o2',
  task: 'p1',
  input_name: 'nixpkgs',
  url: null,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
};

const overrideGlob: FlakeInputOverrideOut = {
  id: 'o3',
  task: 'p1',
  input_name: 'nixpkgs*',
  url: null,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
};

function setup(
  access: AccessState,
  overrides: FlakeInputOverrideOut[] = [override],
  overridesServicePartial: Partial<FlakeInputOverridesService> = {},
): ComponentFixture<TaskFlakeInputsComponent> {
  const defaultService: Partial<FlakeInputOverridesService> = {
    list: () => of(overrides),
    create: () => of(override),
    update: () => of(override),
    delete: () => of({ deleted: true }),
    ...overridesServicePartial,
  };

  TestBed.configureTestingModule({
    imports: [TaskFlakeInputsComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: activatedRouteStub(access) },
      { provide: FlakeInputOverridesService, useValue: defaultService },
      { provide: ProjectsService, useValue: { getProject: () => of({ display_name: 'Acme' }) } },
      { provide: TasksService, useValue: { getTaskInfo: () => of({ display_name: 'Demo Task' }) } },
    ],
  });
  const fixture = TestBed.createComponent(TaskFlakeInputsComponent);
  fixture.detectChanges();
  return fixture;
}

describe('TaskFlakeInputsComponent', () => {
  it('lists overrides on init', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [override]);
    const text = fixture.nativeElement.textContent ?? '';
    expect(text).toContain('utils');
    expect(text).toContain('github:numtide/flake-utils');
  });

  it('marks glob input names as patterns and shows the badge', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [overrideGlob]);
    const comp = fixture.componentInstance;
    expect(comp.isPattern(overrideGlob)).toBe(true);
    expect(comp.isPattern(override)).toBe(false);
    expect(fixture.nativeElement.textContent ?? '').toContain('Pattern');
  });

  it('keep_url checkbox causes url: null submission', () => {
    let capturedBody: any;
    const svc: Partial<FlakeInputOverridesService> = {
      list: () => of([]),
      create: (project, proj, body) => { capturedBody = body; return of(override); },
    };
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [], svc);
    const comp = fixture.componentInstance;

    comp.form.input_name = 'utils';
    comp.form.url = 'ignored';
    comp.form.keepUrl = true;
    comp.saveCreate();

    expect(capturedBody).toEqual({ input_name: 'utils', url: null });
  });

  it('non-keep URL submission passes the URL', () => {
    let capturedBody: any;
    const svc: Partial<FlakeInputOverridesService> = {
      list: () => of([]),
      create: (project, proj, body) => { capturedBody = body; return of(override); },
    };
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [], svc);
    const comp = fixture.componentInstance;

    comp.form.input_name = 'utils';
    comp.form.url = 'github:x/y';
    comp.form.keepUrl = false;
    comp.saveCreate();

    expect(capturedBody).toEqual({ input_name: 'utils', url: 'github:x/y' });
  });

  it('edit prefills form with row values', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [overrideKeepUrl]);
    const comp = fixture.componentInstance;

    comp.startEdit(overrideKeepUrl);

    expect(comp.form.input_name).toBe('nixpkgs');
    expect(comp.form.keepUrl).toBe(true);
    expect(comp.editingId()).toBe('o2');
  });

  it('delete calls service after confirm', () => {
    let deletedId: string | undefined;
    const svc: Partial<FlakeInputOverridesService> = {
      list: () => of([override]),
      delete: (project, proj, id) => { deletedId = id; return of({ deleted: true }); },
    };
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [override], svc);
    const comp = fixture.componentInstance;

    const origConfirm = window.confirm;
    window.confirm = () => true;
    comp.deleteOverride('o1');
    window.confirm = origConfirm;

    expect(deletedId).toBe('o1');
  });

  it('hides write buttons under read-only access', () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false }, [override]);
    const buttons = Array.from(fixture.nativeElement.querySelectorAll('button')) as HTMLButtonElement[];
    const editBtn = buttons.find((b) => (b.textContent ?? '').trim().toLowerCase().includes('edit'));
    const deleteBtn = buttons.find((b) => (b.textContent ?? '').trim().toLowerCase().includes('delete'));
    expect(editBtn).toBeUndefined();
    expect(deleteBtn).toBeUndefined();
  });
});

describe('TaskFlakeInputsComponent - load errors', () => {
  it('shows a failed load as an error, never as the empty state and loads again on Try Again', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, []);
    const service = TestBed.inject(FlakeInputOverridesService) as unknown as { list: () => unknown };
    service.list = () => throwError(() => new Error('database is down'));
    fixture.componentInstance.loadOverrides();
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.textContent).toContain('Failed to load flake inputs');
    expect(root.textContent).toContain('database is down');
    expect(root.textContent).not.toContain('No overrides configured');

    service.list = () => of([]);
    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')!.click();
    fixture.detectChanges();
    expect(root.textContent).toContain('No overrides configured');
  });
});

describe('TaskFlakeInputsComponent - write failures', () => {
  const writer = { managed: false, canEdit: true, canTrigger: true };

  it('reports a failed delete with the server reason and a successful one', () => {
    vi.spyOn(window, 'confirm').mockReturnValue(true);
    const refused = new ApiError('You need write access to change overrides', 403);
    const fixture = setup(writer, [override], { delete: () => throwError(() => refused) });
    const notify = TestBed.inject(NotifyService);
    const failure = vi.spyOn(notify, 'failure');
    const success = vi.spyOn(notify, 'success');
    fixture.componentInstance.deleteOverride('o1');
    expect(failure).toHaveBeenCalledWith('Failed to delete the override for the flake input', refused);
    expect(fixture.componentInstance.deletingId()).toBeNull();

    (TestBed.inject(FlakeInputOverridesService) as unknown as Record<string, unknown>)['delete'] = () => of({ deleted: true });
    fixture.componentInstance.deleteOverride('o1');
    expect(success).toHaveBeenCalledWith('Deleted the override for the flake input');
  });

  it('shows a rejected edit inside the open dialog instead of raw transport text', () => {
    const fixture = setup(writer, [override], {
      update: () => throwError(() => new Error('Http failure response for /overrides/o1: 500 Internal Server Error')),
    });
    const component = fixture.componentInstance;
    component.startEdit(override);
    component.saveEdit();
    fixture.detectChanges();

    expect(component.showEditDialog()).toBe(true);
    const banner = document.body.querySelector('.gr-dialog gr-message-banner');
    expect(banner?.textContent).toContain('Failed to update override.');
    expect(document.body.textContent).not.toContain('Http failure response');
  });
});
