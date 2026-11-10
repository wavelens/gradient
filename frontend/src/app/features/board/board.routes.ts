/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { inject } from '@angular/core';
import { CanActivateFn, Routes } from '@angular/router';
import { map } from 'rxjs';
import { adminGuard } from '@core/guards/admin.guard';
import { denyOnRefusal } from '@core/routing/access-denied';
import { BoardService } from '@core/services/board.service';
import { links } from '@shared/routing';

export const assignedJobDetailGuard: CanActivateFn = (route, state) => {
  const id = route.paramMap.get('id') ?? '';
  return inject(BoardService).getJob(id).pipe(
    map(() => true as const),
    denyOnRefusal({ kind: 'job', back: String(links.board()) }, state.url),
  );
};

export const clusterJobBoardGuard: CanActivateFn = (route, state) => {
  const id = route.paramMap.get('id') ?? '';
  return inject(BoardService).getClusterJob(id).pipe(
    map(() => true as const),
    denyOnRefusal({ kind: 'clusterJob', back: String(links.board()) }, state.url),
  );
};

export const BOARD_ROUTES: Routes = [
  {
    path: 'jobs/:id',
    canActivate: [assignedJobDetailGuard],
    title: 'Assigned job',
    loadComponent: () => import('./job-detail/job-detail.component').then((m) => m.BoardJobDetailComponent),
  },
  {
    path: 'pending/:id',
    title: 'Pending job',
    data: { pending: true },
    loadComponent: () => import('./job-detail/job-detail.component').then((m) => m.BoardJobDetailComponent),
  },
  {
    path: 'clusters/:id',
    canActivate: [clusterJobBoardGuard],
    title: 'Cluster Job',
    loadComponent: () => import('./cluster-detail/cluster-detail.component').then((m) => m.BoardClusterDetailComponent),
  },
  {
    path: '',
    title: 'Job Board',
    loadComponent: () => import('./board-layout.component').then((m) => m.BoardLayoutComponent),
    children: [
      { path: '', pathMatch: 'full', redirectTo: 'overview' },
      {
        path: 'overview',
        title: 'Overview · Job Board',
        loadComponent: () => import('./overview/overview.component').then((m) => m.BoardOverviewComponent),
      },
      {
        path: 'live',
        title: 'Live Jobs · Job Board',
        loadComponent: () => import('./live-jobs/live-jobs.component').then((m) => m.BoardLiveJobsComponent),
      },
      {
        path: 'scheduler',
        title: 'Scheduler · Job Board',
        canActivate: [adminGuard],
        loadComponent: () => import('./scheduler/scheduler.component').then((m) => m.BoardSchedulerComponent),
      },
      {
        path: 'throughput',
        title: 'Throughput · Job Board',
        loadComponent: () => import('./throughput/throughput.component').then((m) => m.BoardThroughputComponent),
      },
      {
        path: 'durations',
        title: 'Durations · Job Board',
        loadComponent: () => import('./durations/durations.component').then((m) => m.BoardDurationsComponent),
      },
      {
        path: 'workers',
        title: 'Workers · Job Board',
        loadComponent: () => import('./workers/workers.component').then((m) => m.BoardWorkersComponent),
      },
      {
        path: 'cache',
        title: 'Cache · Job Board',
        loadComponent: () => import('./cache/cache.component').then((m) => m.BoardCacheComponent),
      },
      {
        path: 'network',
        title: 'Network · Job Board',
        loadComponent: () => import('./network/network.component').then((m) => m.BoardNetworkComponent),
      },
      {
        path: 'health',
        title: 'System Health · Job Board',
        canActivate: [adminGuard],
        loadComponent: () => import('./health/health.component').then((m) => m.BoardHealthComponent),
      },
      {
        path: 'storage',
        title: 'Storage · Job Board',
        canActivate: [adminGuard],
        loadComponent: () => import('./storage/storage.component').then((m) => m.BoardStorageComponent),
      },
      {
        path: 'expensive',
        title: 'High Cost · Job Board',
        loadComponent: () => import('./high-cost/high-cost.component').then((m) => m.BoardHighCostComponent),
      },
      {
        path: 'expensive-evals',
        redirectTo: '/board/expensive?highCostRanking=evaluations',
      },
    ],
  },
];
