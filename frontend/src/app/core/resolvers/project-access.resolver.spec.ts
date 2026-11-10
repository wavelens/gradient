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
import { AuthService } from '@core/services/auth.service';
import { projectAccessResolver, ProjectAccessData } from './project-access.resolver';
import { ProjectResponse } from '@core/models';
import { links } from '@shared/routing';

function snap(params: Record<string, string>): ActivatedRouteSnapshot {
  return { paramMap: convertToParamMap(params) } as ActivatedRouteSnapshot;
}

function runResolver(route: ActivatedRouteSnapshot): Promise<ProjectAccessData> {
  const result = TestBed.runInInjectionContext(() =>
    projectAccessResolver(route, { url: '/project/missing' } as never),
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
    restrict_evaluation_fetches: true,
    extra_allowed_uri_prefixes: [],
    managed: false,
    created_by: 'u1',
    created_at: '2026-01-01T00:00:00',
    github_app_available: false,
    permissions: [],
  };

  let auth: {
    initialized$: Observable<boolean>;
    isAuthenticated: () => boolean;
    stillSignedIn: ReturnType<typeof vi.fn>;
    forgetRejectedSessionAndGetSignInPageUrl: ReturnType<typeof vi.fn>;
  };

  beforeEach(() => {
    getProject = vi.fn(() => of(baseProject));
    auth = {
      initialized$: of(true),
      isAuthenticated: () => true,
      stillSignedIn: vi.fn(() => of(true)),
      forgetRejectedSessionAndGetSignInPageUrl: vi.fn((next: string) => links.login(next, 'expired')),
    };
    TestBed.configureTestingModule({
      providers: [
        provideRouter([]),
        { provide: ProjectsService, useValue: { getProject } },
        { provide: AuthService, useValue: auth },
      ],
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
    expect(result.navigationBehaviorOptions?.browserUrl).toBe('/project/missing');
    expect(TestBed.inject(Router).serializeUrl(result.redirectTo)).toContain('/error/404?kind=project&name=missing&back=%2Fprojects');
  });

  it('leaves a rejected token to the sign-in redirect of the error interceptor', async () => {
    getProject.mockReturnValue(throwError(() => new ApiError('Unable to decode token', 401)));
    const result = TestBed.runInInjectionContext(() =>
      projectAccessResolver(snap({ project: 'wavelens' }), { url: '/project/wavelens' } as never),
    ) as Observable<ProjectAccessData>;
    const emitted = vi.fn();
    result.subscribe({ next: emitted });
    expect(emitted).not.toHaveBeenCalled();
  });

  it('sends a user whose session expired to sign-in and back to the project', async () => {
    getProject.mockReturnValue(throwError(() => new ApiError('Project not found', 404)));
    auth.stillSignedIn.mockReturnValue(of(false));
    const result = (await runResolver(snap({ project: 'missing' }))) as unknown as RedirectCommand;
    expect(auth.forgetRejectedSessionAndGetSignInPageUrl).toHaveBeenCalledWith('/project/missing');
    expect(TestBed.inject(Router).serializeUrl(result.redirectTo)).toBe('/account/login?next=%2Fproject%2Fmissing&reason=expired');
  });
});
