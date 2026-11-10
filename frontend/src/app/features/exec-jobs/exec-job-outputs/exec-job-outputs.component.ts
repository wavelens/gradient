/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, DOCUMENT, computed, inject, input } from '@angular/core';
import { toObservable, toSignal } from '@angular/core/rxjs-interop';
import { EMPTY, Observable, catchError, of, switchMap, takeWhile } from 'rxjs';
import { formatBytes, ServerTimestampToLocalDateTimePipe } from '@shared/text';
import { refreshImmediatelyThenOnIntervalAndOnTabVisibleWhileTabVisible } from '@shared/paging';
import { ApiError } from '@core/services/api.service';
import { ExecJobsService } from '@core/services/exec-jobs.service';
import type { ExecJobOutputView, ExecJobResponse } from '@core/models';
import { isOpenExecJob } from '../exec-job-status';

const POLL_MS = 2000;

interface PollTarget {
  id: string;
  node?: number;
  open: boolean;
}

const sameTarget = (a: PollTarget, b: PollTarget): boolean => a.id === b.id && a.node === b.node && a.open === b.open;

@Component({
  selector: 'app-exec-job-outputs',
  standalone: true,
  imports: [ServerTimestampToLocalDateTimePipe],
  templateUrl: './exec-job-outputs.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrls: ['../../../shared/log/log-sidebar.scss', './exec-job-outputs.component.scss'],
})
export class ExecJobOutputsComponent {
  private execJobs = inject(ExecJobsService);
  private document = inject(DOCUMENT);

  job = input.required<ExecJobResponse>();
  node = input<number>();

  private target = computed<PollTarget>(
    () => ({ id: this.job().id, node: this.node(), open: isOpenExecJob(this.job().status) }),
    { equal: sameTarget },
  );

  outputs = toSignal(toObservable(this.target).pipe(switchMap((target) => this.watch(target))), {
    initialValue: [] as ExecJobOutputView[],
  });

  protected readonly size = formatBytes;

  url(path: string): string {
    return this.execJobs.outputUrl(this.job().id, path, this.node());
  }

  private watch({ id, node, open }: PollTarget): Observable<ExecJobOutputView[]> {
    const ticks = open ? refreshImmediatelyThenOnIntervalAndOnTabVisibleWhileTabVisible(this.document, POLL_MS) : of(0);
    return ticks.pipe(
      switchMap(() => this.execJobs.outputs(id, node).pipe(
        catchError((error: unknown) => (error instanceof ApiError && !error.unreachable ? of(null) : EMPTY)),
      )),
      takeWhile((outputs): outputs is ExecJobOutputView[] => outputs !== null),
    );
  }
}
