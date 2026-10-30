/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { of } from 'rxjs';
import type { ClusterJobDetail } from '@core/models';
import { BoardService } from '@core/services/board.service';
import { BoardClusterDetailComponent, subJobRows } from './cluster-detail.component';

const node = (index: number) => ({
  id: `x${index}`,
  project: 'infra',
  task: 'nightly',
  entry_point: 'apps.x86_64-linux.train',
  node_index: index,
  nodes: 2,
});

const subJob = (id: string, member: string, attempt: number) => ({
  id,
  member,
  attempt,
  worker_id: `w-${id}`,
  worker_name: null,
  dispatched_at: '2026-10-30T10:00:00Z',
  finished_at: null,
  outcome: null,
});

const CLUSTER: ClusterJobDetail = {
  id: 'c1',
  kind: 2,
  status: 'running',
  same_zone: true,
  attempts: 2,
  retry_budget: 1,
  created_at: '2026-10-30T09:00:00Z',
  updated_at: '2026-10-30T10:00:00Z',
  project: 'p1',
  project_name: 'infra',
  project_display_name: 'Infra',
  evaluation_id: 'e1',
  subject: 'apps.x86_64-linux.train',
  members: [
    { id: 'm0', role: 'node', primary: true, exec_job: node(0) },
    { id: 'm1', role: 'node', primary: false, exec_job: node(1) },
  ],
  sub_jobs: [subJob('old', 'm0', 1), subJob('new', 'm0', 2)],
};

describe('BoardClusterDetailComponent', () => {
  it('lists each member with its newest sub job first and a waiting member without one', () => {
    expect(subJobRows(CLUSTER).map((row) => [row.member, row.job?.id ?? null])).toEqual([
      ['Node 0', 'new'],
      ['Node 0', 'old'],
      ['Node 1', null],
    ]);
  });

  it('opens assigned sub jobs and shows a waiting member as plain row', () => {
    TestBed.configureTestingModule({
      imports: [BoardClusterDetailComponent],
      providers: [
        provideRouter([]),
        { provide: ActivatedRoute, useValue: { paramMap: of(convertToParamMap({ id: 'c1' })) } },
        { provide: BoardService, useValue: { getClusterJob: () => of(CLUSTER) } },
      ],
    });
    const fixture = TestBed.createComponent(BoardClusterDetailComponent);
    fixture.detectChanges();

    const root = fixture.nativeElement as HTMLElement;
    const rows = root.querySelectorAll('tbody tr');
    expect(rows.length).toBe(3);
    expect(root.querySelectorAll('tbody tr.clickable').length).toBe(2);
    expect(rows[2].textContent).toContain('Waiting for a worker');
  });
});
