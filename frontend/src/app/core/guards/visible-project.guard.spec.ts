/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import {
  ActivatedRouteSnapshot,
  RedirectCommand,
  RouterStateSnapshot,
  convertToParamMap,
  provideRouter,
} from '@angular/router';
import { Observable, firstValueFrom, isObservable, of, throwError } from 'rxjs';
import { visibleProjectGuard } from './visible-project.guard';
import { ApiError } from '@core/services/api.service';
import { AuthService } from '@core/services/auth.service';
import { ProjectsService } from '@core/services/projects.service';
import { ProjectResponse } from '@core/models';

const REQUESTED_URL = '/project/acme/task/demo?eval=e1';

async function run(session: { authenticated: boolean; unreachable?: boolean }, project: Observable<ProjectResponse>) {
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
  return { outcome, getProject };
}

describe('visibleProjectGuard', () => {
  const notFound = throwError(() => new ApiError('Project not found', 404));

  it('asks the server for signed-in callers too, so a hidden project gets the not-found page', async () => {
    const { outcome, getProject } = await run({ authenticated: true }, notFound);
    expect(getProject).toHaveBeenCalledWith('acme');
    expect(outcome).toBeInstanceOf(RedirectCommand);
  });

  it('lets an anonymous visitor into a public project', async () => {
    const { outcome, getProject } = await run({ authenticated: false }, of({ name: 'acme', public: true } as ProjectResponse));
    expect(outcome).toBe(true);
    expect(getProject).toHaveBeenCalledWith('acme');
  });

  it('cancels the navigation rather than asking for a login when the server is unreachable', async () => {
    const { outcome, getProject } = await run({ authenticated: false, unreachable: true }, notFound);
    expect(outcome).toBe(false);
    expect(getProject).not.toHaveBeenCalled();
  });
});
