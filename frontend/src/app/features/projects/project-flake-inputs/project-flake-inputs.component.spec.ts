/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { of } from 'rxjs';
import { ProjectFlakeInputsComponent } from './project-flake-inputs.component';
import { FlakeInputOverridesService } from '@core/services/flake-input-overrides.service';
import { OrganizationsService } from '@core/services/organizations.service';
import { AccessState } from '@core/models/access.model';
import { FlakeInputOverride } from '@core/models';

function activatedRouteStub(access: AccessState): ActivatedRoute {
  return {
    snapshot: { paramMap: convertToParamMap({ org: 'acme', project: 'demo' }) },
    data: of({}),
    parent: { data: of({ projectAccess: { project: {}, access } }) },
  } as unknown as ActivatedRoute;
}

const override: FlakeInputOverride = {
  id: 'o1',
  project: 'p1',
  input_name: 'utils',
  url: 'github:numtide/flake-utils',
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
};

const overrideKeepUrl: FlakeInputOverride = {
  id: 'o2',
  project: 'p1',
  input_name: 'nixpkgs',
  url: null,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
};

function setup(
  access: AccessState,
  overrides: FlakeInputOverride[] = [override],
  overridesServicePartial: Partial<FlakeInputOverridesService> = {},
): ComponentFixture<ProjectFlakeInputsComponent> {
  const defaultService: Partial<FlakeInputOverridesService> = {
    list: () => of(overrides),
    create: () => of(override),
    update: () => of(override),
    delete: () => of(true),
    ...overridesServicePartial,
  };

  TestBed.configureTestingModule({
    imports: [ProjectFlakeInputsComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: activatedRouteStub(access) },
      { provide: FlakeInputOverridesService, useValue: defaultService },
      { provide: OrganizationsService, useValue: { getOrganization: () => of({ display_name: 'Acme' }) } },
    ],
  });
  const fixture = TestBed.createComponent(ProjectFlakeInputsComponent);
  fixture.detectChanges();
  return fixture;
}

describe('ProjectFlakeInputsComponent', () => {
  it('lists overrides on init', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [override]);
    const text = fixture.nativeElement.textContent ?? '';
    expect(text).toContain('utils');
    expect(text).toContain('github:numtide/flake-utils');
  });

  it('keep_url checkbox causes url: null submission', () => {
    let capturedBody: any;
    const svc: Partial<FlakeInputOverridesService> = {
      list: () => of([]),
      create: (org, proj, body) => { capturedBody = body; return of(override); },
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
      create: (org, proj, body) => { capturedBody = body; return of(override); },
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
      delete: (org, proj, id) => { deletedId = id; return of(true); },
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
