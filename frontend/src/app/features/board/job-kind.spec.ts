/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { BoardExecJob } from '@core/models';
import { collapseClusters, jobKindName, jobSubjectLabel } from './job-kind';

const NODE: BoardExecJob = {
  id: 'x1',
  project: 'infra',
  task: 'nightly',
  task_display_name: 'Nightly',
  entry_point: 'apps.x86_64-linux.train',
  node_index: 1,
  nodes: 2,
};

describe('job kind labels', () => {
  it('names every scheduler job kind', () => {
    expect([0, 1, 2].map(jobKindName)).toEqual(['eval', 'build', 'exec']);
  });

  it('names an exec node by entry point and node, leaving the task to its own column', () => {
    expect(jobSubjectLabel('apps.x86_64-linux.train', NODE)).toBe('apps.x86_64-linux.train (node 1)');
    expect(jobSubjectLabel(null, { ...NODE, node_index: 0, nodes: 1 })).toBe('apps.x86_64-linux.train');
  });

  it('falls back to the plain subject without an exec job', () => {
    expect(jobSubjectLabel('hello-2.12.1', null)).toBe('hello-2.12.1');
    expect(jobSubjectLabel(null, undefined)).toBe('-');
  });

});

describe('cluster jobs on the board', () => {
  it('lists the sub jobs of a cluster as one row at the place of the first', () => {
    const jobs = [
      { id: 'a', cluster_job: 'c1' },
      { id: 'b', cluster_job: null },
      { id: 'c', cluster_job: 'c1' },
      { id: 'd' },
    ];
    expect(collapseClusters(jobs).map((row) => [row.job.id, row.subJobs])).toEqual([
      ['a', 2],
      ['b', 1],
      ['d', 1],
    ]);
  });
});
