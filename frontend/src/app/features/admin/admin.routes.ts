/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Routes } from '@angular/router';

export const ADMIN_ROUTES: Routes = [
  {
    path: '',
    title: 'Administration',
    loadComponent: () =>
      import('./instance-admin-area-shell.component').then((m) => m.InstanceAdminAreaShellComponent),
    children: [
      { path: '', pathMatch: 'full', redirectTo: 'overview' },
      {
        path: 'overview',
        title: 'Overview · Administration',
        loadComponent: () =>
          import('./overview/admin-overview.component').then((m) => m.AdminStatusTilesWithMaintenanceActionsComponent),
      },
      {
        path: 'workers',
        title: 'Workers · Administration',
        loadComponent: () => import('./workers/admin-workers-tab.component').then((m) => m.AdminWorkersTabComponent),
      },
      {
        path: 'storage',
        title: 'Storage · Administration',
        loadComponent: () => import('./storage/admin-storage-tab.component').then((m) => m.AdminStorageTabComponent),
      },
      {
        path: 'settings',
        title: 'Settings · Administration',
        loadComponent: () => import('./settings/admin-settings.component').then((m) => m.AdminSettingsComponent),
      },
    ],
  },
];
