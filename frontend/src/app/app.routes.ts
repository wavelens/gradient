/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Routes } from '@angular/router';
import { authGuard } from '@core/guards/auth.guard';

export const routes: Routes = [
  // Authentication routes (public)
  {
    path: 'account',
    children: [
      {
        path: 'login',
        loadComponent: () =>
          import('./features/auth/login/login.component').then((m) => m.LoginComponent),
      },
      {
        path: 'register',
        loadComponent: () =>
          import('./features/auth/register/register.component').then((m) => m.RegisterComponent),
      },
    ],
  },

  // Protected routes (require authentication)
  {
    path: '',
    canActivate: [authGuard],
    children: [
      // Dashboard
      {
        path: '',
        loadComponent: () =>
          import('./features/dashboard/dashboard.component').then((m) => m.DashboardComponent),
      },

      // Organizations
      {
        path: 'organizations',
        loadComponent: () =>
          import('./features/organizations/organization-list/organization-list.component').then(
            (m) => m.OrganizationListComponent
          ),
      },
      {
        path: 'organization/:org',
        loadComponent: () =>
          import('./features/organizations/organization-detail/organization-detail.component').then(
            (m) => m.OrganizationDetailComponent
          ),
      },
      {
        path: 'organization/:org/settings',
        loadComponent: () =>
          import('./features/organizations/organization-settings/organization-settings.component').then(
            (m) => m.OrganizationSettingsComponent
          ),
      },

      // Projects
      {
        path: 'organization/:org/project/:project',
        loadComponent: () =>
          import('./features/projects/project-detail/project-detail.component').then(
            (m) => m.ProjectDetailComponent
          ),
      },
      {
        path: 'organization/:org/project/:project/settings',
        loadComponent: () =>
          import('./features/projects/project-settings/project-settings.component').then(
            (m) => m.ProjectSettingsComponent
          ),
      },

      // Evaluations
      {
        path: 'organization/:org/log/:evaluationId',
        loadComponent: () =>
          import('./features/evaluations/evaluation-log/evaluation-log.component').then(
            (m) => m.EvaluationLogComponent
          ),
      },
      {
        path: 'organization/:org/log',
        loadComponent: () =>
          import('./features/evaluations/evaluation-log/evaluation-log.component').then(
            (m) => m.EvaluationLogComponent
          ),
      },

      // Caches
      {
        path: 'caches',
        loadComponent: () =>
          import('./features/caches/cache-list/cache-list.component').then(
            (m) => m.CacheListComponent
          ),
      },
      {
        path: 'caches/:cache',
        loadComponent: () =>
          import('./features/caches/cache-detail/cache-detail.component').then(
            (m) => m.CacheDetailComponent
          ),
      },

      // Settings
      {
        path: 'settings/profile',
        loadComponent: () =>
          import('./features/settings/profile/profile.component').then(
            (m) => m.ProfileComponent
          ),
      },
      {
        path: 'settings/keys',
        loadComponent: () =>
          import('./features/settings/api-keys/api-keys.component').then(
            (m) => m.ApiKeysComponent
          ),
      },
    ],
  },

  // Fallback route
  {
    path: '**',
    redirectTo: '',
  },
];
