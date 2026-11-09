/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRouteSnapshot, RedirectCommand, Router, convertToParamMap, provideRouter } from '@angular/router';
import { Observable, firstValueFrom, of } from 'rxjs';
import { AuthService } from '@core/services/auth.service';
import { EvaluationsService } from '@core/services/evaluations.service';
import { TasksService } from '@core/services/tasks.service';
import { EvaluationResponse, TaskDetailsResponse } from '@core/models';
import { EvaluationWithOptionalTask, evaluationAccessResolver } from './evaluation-access.resolver';

const evaluation = { id: 'e1', task: 't1', task_name: 'hosts', project_name: 'infra' } as EvaluationResponse;

function resolve(project: string, url: string): Promise<EvaluationWithOptionalTask | RedirectCommand> {
  const route = { paramMap: convertToParamMap({ project, evaluationId: 'e1' }) } as ActivatedRouteSnapshot;
  const result = TestBed.runInInjectionContext(() => evaluationAccessResolver(route, { url } as never));
  return firstValueFrom(result as Observable<EvaluationWithOptionalTask | RedirectCommand>);
}

describe('evaluationAccessResolver', () => {
  let getTask: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    getTask = vi.fn(() => of({ id: 't1', name: 'hosts' } as TaskDetailsResponse));
    TestBed.configureTestingModule({
      providers: [
        provideRouter([]),
        { provide: EvaluationsService, useValue: { getEvaluation: () => of(evaluation) } },
        { provide: TasksService, useValue: { getTask } },
        { provide: AuthService, useValue: { initialized$: of(true), isAuthenticated: () => true } },
      ],
    });
  });

  it('loads the evaluation with its task under the right project', async () => {
    const data = (await resolve('infra', '/project/infra/log/e1')) as EvaluationWithOptionalTask;
    expect(getTask).toHaveBeenCalledWith('infra', 'hosts');
    expect(data.task?.id).toBe('t1');
  });

  it('redirects a link with an old project name to the same page under the current name', async () => {
    const result = await resolve('old-infra', '/project/old-infra/log/e1?build=b1');
    expect(result).toBeInstanceOf(RedirectCommand);
    expect(TestBed.inject(Router).serializeUrl((result as RedirectCommand).redirectTo)).toBe('/project/infra/log/e1?build=b1');
    expect(getTask).not.toHaveBeenCalled();
  });
});
