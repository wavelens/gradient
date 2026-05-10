/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRouteSnapshot, convertToParamMap } from '@angular/router';
import { Observable, firstValueFrom, of } from 'rxjs';
import { ProjectsService } from '@core/services/projects.service';
import {
  projectAccessResolver,
  ProjectAccessData,
} from './project-access.resolver';
import { ProjectDetail } from '@core/models/project.model';

function snap(params: Record<string, string>): ActivatedRouteSnapshot {
  return { paramMap: convertToParamMap(params) } as ActivatedRouteSnapshot;
}

function runResolver(
  route: ActivatedRouteSnapshot,
): Promise<ProjectAccessData> {
  const result = TestBed.runInInjectionContext(() =>
    projectAccessResolver(route, {} as never),
  ) as Observable<ProjectAccessData>;
  return firstValueFrom(result);
}

describe('projectAccessResolver', () => {
  let getProject: ReturnType<typeof vi.fn>;

  const baseProject: ProjectDetail = {
    id: 'p1',
    name: 'demo',
    display_name: 'Demo',
    description: '',
    repository: '',
    wildcard: '',
    active: true,
    created_at: '',
    keep_evaluations: 5,
    last_evaluations: [],
    can_edit: true,
    managed: false,
  };

  beforeEach(() => {
    getProject = vi.fn(() => of(baseProject));
    TestBed.configureTestingModule({
      providers: [{ provide: ProjectsService, useValue: { getProject } }],
    });
  });

  it('fetches the project by route params and exposes access state', async () => {
    const data = await runResolver(snap({ org: 'acme', project: 'demo' }));
    expect(getProject).toHaveBeenCalledWith('acme', 'demo');
    expect(data.project).toBe(baseProject);
    expect(data.access).toEqual({ managed: false, canEdit: true });
  });

  it('propagates managed=true and can_edit=false into access', async () => {
    getProject.mockReturnValue(
      of({ ...baseProject, managed: true, can_edit: false }),
    );
    const data = await runResolver(snap({ org: 'acme', project: 'demo' }));
    expect(data.access).toEqual({ managed: true, canEdit: false });
  });
});
