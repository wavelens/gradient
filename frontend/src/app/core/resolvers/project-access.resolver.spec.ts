/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRouteSnapshot, convertToParamMap } from '@angular/router';
import { Observable, firstValueFrom, of, throwError } from 'rxjs';
import { ProjectsService } from '@core/services/projects.service';
import { projectAccessResolver, ProjectAccessData } from './project-access.resolver';
import { Project } from '@core/models';

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

  const baseProject: Project = {
    id: 'o1',
    name: 'wavelens',
    display_name: 'Wavelens',
    description: '',
    public: true,
    hide_build_requests: false,
    managed: false,
  };

  beforeEach(() => {
    getProject = vi.fn(() => of(baseProject));
    TestBed.configureTestingModule({
      providers: [{ provide: ProjectsService, useValue: { getProject } }],
    });
  });

  it('fetches the project by route param', async () => {
    const data = await runResolver(snap({ project: 'wavelens' }));
    expect(getProject).toHaveBeenCalledWith('wavelens');
    expect(data.project).toBe(baseProject);
  });

  it('returns null when the project param is missing', async () => {
    const data = await runResolver(snap({}));
    expect(getProject).not.toHaveBeenCalled();
    expect(data.project).toBeNull();
  });

  it('falls back to null when the fetch errors so navigation still proceeds', async () => {
    getProject.mockReturnValue(throwError(() => new Error('boom')));
    const data = await runResolver(snap({ project: 'missing' }));
    expect(data.project).toBeNull();
  });
});
