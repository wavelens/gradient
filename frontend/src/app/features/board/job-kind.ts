/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { BoardExecJob } from '@core/models';

export type JobKindName = 'eval' | 'build' | 'exec';

const KIND_NAMES: JobKindName[] = ['eval', 'build', 'exec'];

export const EXEC_KIND = KIND_NAMES.indexOf('exec');

export function jobKindName(kind: number): string {
  return KIND_NAMES[kind] ?? String(kind);
}

export function jobSubjectLabel(subject?: string | null, exec?: BoardExecJob | null): string {
  if (!exec) return subject ?? '-';
  const node = exec.nodes > 1 ? ` (node ${exec.node_index})` : '';
  return `${exec.task}: ${exec.entry_point}${node}`;
}

export function execJobLink(exec: BoardExecJob): string[] {
  return ['/project', exec.project, 'task', exec.task, 'exec-jobs', exec.id];
}

export function boardJobLink(dispatched?: string | null, cluster?: string | null): string[] | undefined {
  if (cluster) return ['/board', 'clusters', cluster];
  return dispatched ? ['/board', 'jobs', dispatched] : undefined;
}

export interface BoardRow<T> {
  job: T;
  subJobs: number;
}

export function collapseClusters<T extends { cluster_job?: string | null }>(jobs: readonly T[]): BoardRow<T>[] {
  const rows: BoardRow<T>[] = [];
  const byCluster = new Map<string, BoardRow<T>>();
  for (const job of jobs) {
    const cluster = job.cluster_job;
    const seen = cluster ? byCluster.get(cluster) : undefined;
    if (seen) {
      seen.subJobs += 1;
      continue;
    }
    const row = { job, subJobs: 1 };
    if (cluster) byCluster.set(cluster, row);
    rows.push(row);
  }
  return rows;
}
