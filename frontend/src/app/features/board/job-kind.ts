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
