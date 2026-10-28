/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, inject, input } from '@angular/core';
import { toObservable, toSignal } from '@angular/core/rxjs-interop';
import { EMPTY, Observable, catchError, interval, of, startWith, switchMap } from 'rxjs';
import { TableComponent } from '@gradient/ui/ui';
import { clockTime, formatBytes } from '@shared/text';
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
  imports: [TableComponent],
  templateUrl: './exec-job-outputs.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './exec-job-outputs.component.scss',
})
export class ExecJobOutputsComponent {
  private execJobs = inject(ExecJobsService);

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
  protected readonly time = clockTime;

  url(path: string): string {
    return this.execJobs.outputUrl(this.job().id, path, this.node());
  }

  private watch({ id, node, open }: PollTarget): Observable<ExecJobOutputView[]> {
    const ticks = open ? interval(POLL_MS).pipe(startWith(0)) : of(0);
    return ticks.pipe(switchMap(() => this.execJobs.outputs(id, node).pipe(catchError(() => EMPTY))));
  }
}
