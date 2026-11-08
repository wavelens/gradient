/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { EMPTY, NEVER, type Observable, of, throwError } from 'rxjs';
import { vi } from 'vitest';
import { TaskFlakeInputsComponent } from './task-flake-inputs.component';
import { NotifyService } from '@core/services/notify.service';
import { FlakeInputOverridesService } from '@core/services/flake-input-overrides.service';
import { ProjectsService } from '@core/services/projects.service';
import { TasksService } from '@core/services/tasks.service';
import { AccessState } from '@core/models/access.model';
import { taskAccessData } from '@core/resolvers/access-route.testing';
import type { FlakeInputOverrideOut, TaskDetailsResponse } from '@core/models';
import { ConfirmService, type ConfirmRequest } from '@shared/confirm';

function activatedRouteStub(access: AccessState, task: Partial<TaskDetailsResponse>): ActivatedRoute {
  return {
    snapshot: { paramMap: convertToParamMap({ project: 'acme', task: 'demo' }) },
    data: of({}),
    parent: { data: of(taskAccessData(access, task)) },
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
  task: Partial<TaskDetailsResponse> = {},
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
      { provide: ActivatedRoute, useValue: activatedRouteStub(access, task) },
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
  it('shows a visitor outside the project no access and asks the server nothing', () => {
    const list = vi.fn(() => of([override]));
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false }, [override], { list }, { permissions: [] });
    const el = fixture.nativeElement as HTMLElement;
    expect(list).not.toHaveBeenCalled();
    expect(el.textContent).toContain('Only the members of this project can view this.');
    expect(el.textContent).not.toContain('Try Again');
  });

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

  function deleteButton(fixture: ComponentFixture<TaskFlakeInputsComponent>): HTMLButtonElement {
    return (Array.from(fixture.nativeElement.querySelectorAll('button')) as HTMLButtonElement[])
      .find((b) => (b.textContent ?? '').trim().toLowerCase().includes('delete'))!;
  }

  it('asks first, naming the input, then deletes the override', () => {
    const confirm = vi.fn((request: ConfirmRequest): Observable<unknown> => request.action());
    TestBed.overrideProvider(ConfirmService, { useValue: { confirm } });
    const remove = vi.fn(() => of({ deleted: true }));
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [override], { delete: remove });
    const success = vi.spyOn(TestBed.inject(NotifyService), 'success');
    deleteButton(fixture).click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ target: 'utils' }));
    expect(remove).toHaveBeenCalledWith('acme', 'demo', 'o1');
    expect(success).toHaveBeenCalledWith('Deleted the override for the flake input');
  });

  it('sends nothing when the delete is cancelled', () => {
    TestBed.overrideProvider(ConfirmService, { useValue: { confirm: vi.fn(() => EMPTY) } });
    const remove = vi.fn(() => of({ deleted: true }));
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [override], { delete: remove });
    deleteButton(fixture).click();
    expect(remove).not.toHaveBeenCalled();
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

  it('keeps the edit dialog open on Escape while the save runs', () => {
    const fixture = setup(writer, [override], { update: () => NEVER });
    const component = fixture.componentInstance;
    component.startEdit(override);
    component.form.url = 'github:numtide/flake-utils/main';
    fixture.detectChanges();
    component.saveEdit();
    fixture.detectChanges();
    const panel = document.querySelector('.gr-dialog') as HTMLElement;
    panel.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', keyCode: 27, bubbles: true }));
    fixture.detectChanges();
    expect(component.showEditDialog()).toBe(true);
    expect(panel.getAttribute('aria-busy')).toBe('true');
  });

  it('shows a rejected edit inside the open dialog instead of raw transport text', () => {
    const fixture = setup(writer, [override], {
      update: () => throwError(() => new Error('Http failure response for /overrides/o1: 500 Internal Server Error')),
    });
    const component = fixture.componentInstance;
    component.startEdit(override);
    component.form.url = 'github:numtide/flake-utils/main';
    component.saveEdit();
    fixture.detectChanges();

    expect(component.showEditDialog()).toBe(true);
    const banner = document.body.querySelector('.gr-dialog gr-message-banner');
    expect(banner?.textContent).toContain('Failed to update override.');
    expect(document.body.textContent).not.toContain('Http failure response');
  });
});

describe('TaskFlakeInputsComponent - validation', () => {
  const writer = { managed: false, canEdit: true, canTrigger: true };
  const fieldErrors = () =>
    Array.from(document.querySelectorAll('.gr-dialog .field-error')).map((e) => e.textContent?.trim());

  function withWrites() {
    const create = vi.fn(() => of(override));
    const update = vi.fn(() => of(override));
    const fixture = setup(writer, [override], { create, update });
    return { fixture, component: fixture.componentInstance, create, update };
  }

  it('names the missing input name and URL and sends nothing', () => {
    const { fixture, component, create } = withWrites();
    component.startCreate();
    component.saveCreate();
    fixture.detectChanges();
    expect(fieldErrors()).toEqual([
      'Enter an input name.',
      'Enter a flake URL, or turn on the option above to use the URL from flake.nix.',
    ]);
    expect(create).not.toHaveBeenCalled();
  });

  it.each([
    ['nix pkgs', 'glob characters *, ?'],
    ['1nixpkgs', 'Start a name'],
    ['utils', 'already has an override'],
  ])('refuses the input name %s', (name, message) => {
    const { fixture, component, create } = withWrites();
    component.startCreate();
    component.form = { input_name: name, url: '', keepUrl: true };
    component.saveCreate();
    fixture.detectChanges();
    expect(fieldErrors()).toEqual([expect.stringContaining(message)]);
    expect(create).not.toHaveBeenCalled();
  });

  it('creates on Enter in the input name field', () => {
    const { fixture, component, create } = withWrites();
    component.startCreate();
    component.form = { input_name: 'nixpkgs*', url: '', keepUrl: true };
    fixture.detectChanges();
    document.querySelector('#fi-input-name')!.dispatchEvent(new KeyboardEvent('keyup', { key: 'Enter' }));
    expect(create).toHaveBeenCalledWith('acme', 'demo', { input_name: 'nixpkgs*', url: null });
  });

  it('closes the edit dialog without a request when nothing changed', () => {
    const { component, update } = withWrites();
    component.startEdit(override);
    component.saveEdit();
    expect(update).not.toHaveBeenCalled();
    expect(component.showEditDialog()).toBe(false);
  });
});
