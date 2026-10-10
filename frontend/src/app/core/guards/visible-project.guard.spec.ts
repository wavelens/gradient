/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import {
  ActivatedRouteSnapshot,
  Router,
  RouterStateSnapshot,
  convertToParamMap,
  provideRouter,
} from '@angular/router';
import { Observable, firstValueFrom, isObservable, of, throwError } from 'rxjs';
import { visibleProjectGuard } from './visible-project.guard';
import { ApiError } from '@core/services/api.service';
import { AuthService } from '@core/services/auth.service';
import { ProjectsService } from '@core/services/projects.service';
import { Project } from '@core/models';

const REQUESTED_URL = '/project/acme/task/demo?eval=e1';

async function run(session: { authenticated: boolean; unreachable?: boolean }, project: Observable<Project>) {
  const getProject = vi.fn(() => project);
  TestBed.configureTestingModule({
    providers: [
      provideRouter([]),
      {
        provide: AuthService,
        useValue: {
          initialized$: of(true),
          resolveSession: () => of(session.authenticated),
          serverUnreachable: () => !!session.unreachable,
        },
      },
      { provide: ProjectsService, useValue: { getProject } },
    ],
  });
  const result = TestBed.runInInjectionContext(() =>
    visibleProjectGuard(
      { paramMap: convertToParamMap({ project: 'acme' }) } as ActivatedRouteSnapshot,
      { url: REQUESTED_URL } as RouterStateSnapshot,
    ),
  );
  const outcome = isObservable(result) ? await firstValueFrom(result) : await result;
  return { outcome, getProject, router: TestBed.inject(Router) };
}

describe('visibleProjectGuard', () => {
  const notFound = throwError(() => new ApiError('Project not found', 404));

  it('lets a logged-in caller through without asking, so a hidden project stays a not-found page', async () => {
    const { outcome, getProject } = await run({ authenticated: true }, notFound);
    expect(outcome).toBe(true);
    expect(getProject).not.toHaveBeenCalled();
  });

  it('lets an anonymous visitor into a public project', async () => {
    const { outcome, getProject } = await run({ authenticated: false }, of({ name: 'acme', public: true } as Project));
    expect(outcome).toBe(true);
    expect(getProject).toHaveBeenCalledWith('acme');
  });

  it('sends an anonymous visitor to login, carrying the requested URL, when the project is private or missing', async () => {
    const { outcome, router } = await run({ authenticated: false }, notFound);
    expect(router.serializeUrl(outcome as ReturnType<Router['parseUrl']>)).toBe(
      `/account/login?next=${encodeURIComponent(REQUESTED_URL)}`,
    );
  });

  it('cancels the navigation rather than asking for a login when the server is unreachable', async () => {
    const { outcome, getProject } = await run({ authenticated: false, unreachable: true }, notFound);
    expect(outcome).toBe(false);
    expect(getProject).not.toHaveBeenCalled();
  });
});
