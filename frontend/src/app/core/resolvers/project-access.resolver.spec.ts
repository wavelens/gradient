/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRouteSnapshot, RedirectCommand, Router, convertToParamMap, provideRouter } from '@angular/router';
import { Observable, firstValueFrom, of, throwError } from 'rxjs';
import { ApiError } from '@core/services/api.service';
import { ProjectsService } from '@core/services/projects.service';
import { projectAccessResolver, ProjectAccessData } from './project-access.resolver';
import { ProjectResponse } from '@core/models';

function snap(params: Record<string, string>): ActivatedRouteSnapshot {
  return { paramMap: convertToParamMap(params) } as ActivatedRouteSnapshot;
}

function runResolver(route: ActivatedRouteSnapshot): Promise<ProjectAccessData> {
  const result = TestBed.runInInjectionContext(() =>
    projectAccessResolver(route, {} as never),
  ) as Observable<ProjectAccessData>;
  return firstValueFrom(result);
}

describe('projectAccessResolver', () => {
  let getProject: ReturnType<typeof vi.fn>;

  const baseProject: ProjectResponse = {
    id: 'o1',
    name: 'wavelens',
    display_name: 'Wavelens',
    description: '',
    public: true,
    hide_build_requests: false,
    allow_exec_in_build_requests: false,
    managed: false,
    created_by: 'u1',
    created_at: '2026-01-01T00:00:00',
    github_app_available: false,
  };

  beforeEach(() => {
    getProject = vi.fn(() => of(baseProject));
    TestBed.configureTestingModule({
      providers: [provideRouter([]), { provide: ProjectsService, useValue: { getProject } }],
    });
  });

  it('fetches the project by route param', async () => {
    const data = await runResolver(snap({ project: 'wavelens' }));
    expect(getProject).toHaveBeenCalledWith('wavelens');
    expect(data.project).toBe(baseProject);
  });

  it('shows the shared not-found page in place for a hidden or missing project', async () => {
    getProject.mockReturnValue(throwError(() => new ApiError('Project not found', 404)));
    const result = (await runResolver(snap({ project: 'missing' }))) as unknown as RedirectCommand;
    expect(result).toBeInstanceOf(RedirectCommand);
    expect(result.navigationBehaviorOptions?.skipLocationChange).toBe(true);
    expect(TestBed.inject(Router).serializeUrl(result.redirectTo)).toContain('/error/404?kind=project&name=missing&back=%2Fprojects');
  });
});
