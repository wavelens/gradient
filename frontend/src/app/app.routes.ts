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
        title: 'Sign In',
        loadComponent: () =>
          import('./features/auth/login/login.component').then((m) => m.LoginComponent),
      },
      {
        path: 'register',
        title: 'Register',
        loadComponent: () =>
          import('./features/auth/register/register.component').then((m) => m.RegisterComponent),
      },
      {
        path: 'oidc-callback',
        loadComponent: () =>
          import('./features/auth/oidc-callback/oidc-callback.component').then(
            (m) => m.OidcCallbackComponent
          ),
      },
    ],
  },

  // Public-browsable routes (no login required, write actions hidden)
  {
    path: 'organization/:org',
    title: 'Organization',
    loadComponent: () =>
      import('./features/organizations/organization-detail/organization-detail.component').then(
        (m) => m.OrganizationDetailComponent
      ),
  },
  {
    path: 'organization/:org/project/:project',
    title: 'Project',
    loadComponent: () =>
      import('./features/projects/project-detail/project-detail.component').then(
        (m) => m.ProjectDetailComponent
      ),
  },
  {
    path: 'organization/:org/project/:project/metrics',
    title: 'Project Metrics',
    loadComponent: () =>
      import('./features/projects/project-metrics/project-metrics.component').then(
        (m) => m.ProjectMetricsComponent
      ),
  },
  {
    path: 'organization/:org/project/:project/entry-point-metrics',
    title: 'Entry Point Metrics',
    loadComponent: () =>
      import('./features/projects/entry-point-metrics/entry-point-metrics.component').then(
        (m) => m.EntryPointMetricsComponent
      ),
  },
  {
    path: 'organization/:org/artefacts/:buildId',
    title: 'Build Artefacts',
    data: { hideFooter: true },
    loadComponent: () =>
      import('./features/evaluations/build-artefacts/build-artefacts.component').then(
        (m) => m.BuildArtefactsComponent
      ),
  },
  {
    path: 'organization/:org/graph/:buildId',
    title: 'Dependency Graph',
    data: { hideFooter: true },
    loadComponent: () =>
      import('./features/evaluations/dependency-graph/dependency-graph.component').then(
        (m) => m.DependencyGraphComponent
      ),
  },
  {
    path: 'organization/:org/log/:evaluationId',
    title: 'Evaluation Log',
    data: { hideFooter: true },
    loadComponent: () =>
      import('./features/evaluations/evaluation-log/evaluation-log.component').then(
        (m) => m.EvaluationLogComponent
      ),
  },
  {
    path: 'organization/:org/log',
    title: 'Evaluation Log',
    data: { hideFooter: true },
    loadComponent: () =>
      import('./features/evaluations/evaluation-log/evaluation-log.component').then(
        (m) => m.EvaluationLogComponent
      ),
  },
  {
    path: 'caches/:cache',
    title: 'Cache',
    loadComponent: () =>
      import('./features/caches/cache-detail/cache-detail.component').then(
        (m) => m.CacheDetailComponent
      ),
  },

  // Public-browsable list routes
  {
    path: 'organizations',
    title: 'Organizations',
    loadComponent: () =>
      import('./features/organizations/organization-list/organization-list.component').then(
        (m) => m.OrganizationListComponent
      ),
  },
  {
    path: 'caches',
    title: 'Caches',
    loadComponent: () =>
      import('./features/caches/cache-list/cache-list.component').then(
        (m) => m.CacheListComponent
      ),
  },

  // Protected routes (require authentication)
  {
    path: '',
    canActivate: [authGuard],
    children: [
      // Dashboard
      {
        path: '',
        title: 'Dashboard',
        loadComponent: () =>
          import('./features/dashboard/dashboard.component').then((m) => m.DashboardComponent),
      },

      // Organizations
      {
        path: 'organization/:org/settings',
        title: 'Organization Settings',
        loadComponent: () =>
          import('./features/organizations/organization-settings/organization-settings.component').then(
            (m) => m.OrganizationSettingsComponent
          ),
      },

      // Servers
      {
        path: 'organization/:org/servers',
        title: 'Servers',
        loadComponent: () =>
          import('./features/organizations/servers/servers.component').then(
            (m) => m.ServersComponent
          ),
      },

      // Cache Subscriptions
      {
        path: 'organization/:org/caches',
        title: 'Cache Subscriptions',
        loadComponent: () =>
          import('./features/organizations/cache-subscriptions/cache-subscriptions.component').then(
            (m) => m.CacheSubscriptionsComponent
          ),
      },

      // Projects
      {
        path: 'organization/:org/project/:project/settings',
        title: 'Project Settings',
        loadComponent: () =>
          import('./features/projects/project-settings/project-settings.component').then(
            (m) => m.ProjectSettingsComponent
          ),
      },

      // Caches
      {
        path: 'caches/:cache/settings',
        title: 'Cache Settings',
        loadComponent: () =>
          import('./features/caches/cache-settings/cache-settings.component').then(
            (m) => m.CacheSettingsComponent
          ),
      },
      {
        path: 'caches/:cache/upstreams',
        title: 'Upstream Caches',
        loadComponent: () =>
          import('./features/caches/cache-upstreams/cache-upstreams.component').then(
            (m) => m.CacheUpstreamsComponent
          ),
      },

      // Settings
      {
        path: 'settings/profile',
        title: 'Profile',
        loadComponent: () =>
          import('./features/settings/profile/profile.component').then(
            (m) => m.ProfileComponent
          ),
      },
      {
        path: 'settings/keys',
        title: 'API Keys',
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
