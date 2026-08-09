/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRouteSnapshot, convertToParamMap } from '@angular/router';
import { Observable, firstValueFrom, of } from 'rxjs';
import { TasksService } from '@core/services/tasks.service';
import {
  taskAccessResolver,
  TaskAccessData,
} from './task-access.resolver';
import { TaskDetail } from '@core/models/task.model';

function snap(params: Record<string, string>): ActivatedRouteSnapshot {
  return { paramMap: convertToParamMap(params) } as ActivatedRouteSnapshot;
}

function runResolver(
  route: ActivatedRouteSnapshot,
): Promise<TaskAccessData> {
  const result = TestBed.runInInjectionContext(() =>
    taskAccessResolver(route, {} as never),
  ) as Observable<TaskAccessData>;
  return firstValueFrom(result);
}

describe('taskAccessResolver', () => {
  let getTask: ReturnType<typeof vi.fn>;

  const baseTask: TaskDetail = {
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
    last_check_at: '',
    queue: { building: 0, queued: 0 },
    can_edit: true,
    can_trigger: true,
    managed: false,
  };

  beforeEach(() => {
    getTask = vi.fn(() => of(baseTask));
    TestBed.configureTestingModule({
      providers: [{ provide: TasksService, useValue: { getTask } }],
    });
  });

  it('fetches the task by route params and exposes access state', async () => {
    const data = await runResolver(snap({ project: 'acme', task: 'demo' }));
    expect(getTask).toHaveBeenCalledWith('acme', 'demo');
    expect(data.task).toBe(baseTask);
    expect(data.access).toEqual({ managed: false, canEdit: true, canTrigger: true });
  });

  it('propagates managed=true and can_edit=false into access', async () => {
    getTask.mockReturnValue(
      of({ ...baseTask, managed: true, can_edit: false, can_trigger: false }),
    );
    const data = await runResolver(snap({ project: 'acme', task: 'demo' }));
    expect(data.access).toEqual({ managed: true, canEdit: false, canTrigger: false });
  });

  it('propagates can_trigger independently of can_edit (TriggerEvaluation-only caller)', async () => {
    getTask.mockReturnValue(
      of({ ...baseTask, managed: true, can_edit: false, can_trigger: true }),
    );
    const data = await runResolver(snap({ project: 'acme', task: 'demo' }));
    expect(data.access).toEqual({ managed: true, canEdit: false, canTrigger: true });
  });
});
