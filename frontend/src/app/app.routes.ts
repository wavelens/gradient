/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Routes } from '@angular/router';
import { authGuard } from '@core/guards/auth.guard';
import { adminGuard } from '@core/guards/admin.guard';
import { unsavedChangesGuard } from '@core/guards/unsaved-changes.guard';
import { taskAccessResolver } from '@core/resolvers/task-access.resolver';
import { cacheAccessResolver } from '@core/resolvers/cache-access.resolver';
import { organizationAccessResolver } from '@core/resolvers/organization-access.resolver';

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
      {
        path: 'cli-authorize',
        title: 'Authorize CLI',
        loadComponent: () =>
          import('./features/auth/cli-authorize/cli-authorize.component').then(
            (m) => m.CliAuthorizeComponent
          ),
      },
    ],
  },

  // Organization detail (public)
  {
    path: 'organization/:org',
    title: 'Organization',
    resolve: { organizationAccess: organizationAccessResolver },
    loadComponent: () =>
      import('./features/organizations/organization-detail/organization-detail.component').then(
        (m) => m.OrganizationDetailComponent
      ),
  },

  // Task tree with parent layout + access resolver
  {
    path: 'organization/:org/task/:task',
    loadComponent: () =>
      import('./features/tasks/task-layout/task-layout.component').then(
        (m) => m.TaskLayoutComponent,
      ),
    resolve: { taskAccess: taskAccessResolver },
    runGuardsAndResolvers: 'paramsChange',
    children: [
      {
        path: '',
        title: 'Task',
        loadComponent: () =>
          import('./features/tasks/task-detail/task-detail.component').then(
            (m) => m.TaskDetailComponent,
          ),
      },
      {
        path: 'metrics',
        title: 'Task Metrics',
        loadComponent: () =>
          import('./features/tasks/task-metrics/task-metrics.component').then(
            (m) => m.TaskMetricsComponent,
          ),
      },
      {
        path: 'entry-point-metrics',
        title: 'Entry Point Metrics',
        loadComponent: () =>
          import('./features/tasks/entry-point-metrics/entry-point-metrics.component').then(
            (m) => m.EntryPointMetricsComponent,
          ),
      },
      {
        path: 'settings',
        title: 'Task Settings',
        canActivate: [authGuard],
        loadComponent: () =>
          import('./features/tasks/task-settings/task-settings.component').then(
            (m) => m.TaskSettingsComponent,
          ),
      },
      {
        path: 'triggers',
        title: 'Task Triggers',
        canActivate: [authGuard],
        loadComponent: () =>
          import('./features/tasks/task-triggers/task-triggers.component').then(
            (m) => m.TaskTriggersComponent,
          ),
      },
      {
        path: 'actions',
        title: 'Task Actions',
        canActivate: [authGuard],
        loadComponent: () =>
          import('./features/tasks/task-actions/task-actions.component').then(
            (m) => m.TaskActionsComponent,
          ),
      },
      {
        path: 'flake-inputs',
        title: 'Task Flake Inputs',
        canActivate: [authGuard],
        loadComponent: () =>
          import('./features/tasks/task-flake-inputs/task-flake-inputs.component').then(
            (m) => m.TaskFlakeInputsComponent,
          ),
      },
    ],
  },

  // Build / evaluation utility routes (no layout)
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
    path: 'organization/:org/closure/:kind/:id',
    title: 'Closure',
    data: { hideFooter: true },
    loadComponent: () =>
      import('./features/evaluations/closure-graph/closure-graph.component').then(
        (m) => m.ClosureGraphComponent
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

  // Cache tree with parent layout + access resolver
  {
    path: 'caches/:cache',
    loadComponent: () =>
      import('./features/caches/cache-layout/cache-layout.component').then(
        (m) => m.CacheLayoutComponent,
      ),
    resolve: { cacheAccess: cacheAccessResolver },
    runGuardsAndResolvers: 'paramsChange',
    children: [
      {
        path: '',
        title: 'Cache',
        loadComponent: () =>
          import('./features/caches/cache-detail/cache-detail.component').then(
            (m) => m.CacheDetailComponent,
          ),
      },
      {
        path: 'settings',
        title: 'Cache Settings',
        canActivate: [authGuard],
        loadComponent: () =>
          import('./features/caches/cache-settings/cache-settings.component').then(
            (m) => m.CacheSettingsComponent,
          ),
      },
      {
        path: 'upstreams',
        title: 'Upstream Caches',
        canActivate: [authGuard],
        loadComponent: () =>
          import('./features/caches/cache-upstreams/cache-upstreams.component').then(
            (m) => m.CacheUpstreamsComponent,
          ),
      },
      {
        path: 'nars',
        title: 'Cache NARs',
        loadComponent: () =>
          import('./features/caches/cache-nars/cache-nars.component').then(
            (m) => m.CacheNarsComponent,
          ),
      },
      {
        path: 'members-roles',
        title: 'Cache Members & Roles',
        canActivate: [authGuard],
        loadComponent: () =>
          import('./features/caches/members-roles/cache-members-roles.component').then(
            (m) => m.CacheMembersRolesComponent,
          ),
      },
    ],
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

      // Job Board
      {
        path: 'board',
        title: 'Job Board',
        loadComponent: () =>
          import('./features/board/board-layout.component').then((m) => m.BoardLayoutComponent),
        children: [
          { path: '', pathMatch: 'full', redirectTo: 'overview' },
          {
            path: 'overview',
            loadComponent: () =>
              import('./features/board/overview/overview.component').then(
                (m) => m.BoardOverviewComponent
              ),
          },
          {
            path: 'live',
            loadComponent: () =>
              import('./features/board/live-jobs/live-jobs.component').then(
                (m) => m.BoardLiveJobsComponent
              ),
          },
          {
            path: 'jobs/:id',
            title: 'Dispatched Job',
            loadComponent: () =>
              import('./features/board/job-detail/job-detail.component').then(
                (m) => m.BoardJobDetailComponent
              ),
          },
          {
            path: 'scheduler',
            loadComponent: () =>
              import('./features/board/scheduler/scheduler.component').then(
                (m) => m.BoardSchedulerComponent
              ),
          },
          {
            path: 'throughput',
            loadComponent: () =>
              import('./features/board/throughput/throughput.component').then(
                (m) => m.BoardThroughputComponent
              ),
          },
          {
            path: 'durations',
            loadComponent: () =>
              import('./features/board/durations/durations.component').then(
                (m) => m.BoardDurationsComponent
              ),
          },
          {
            path: 'workers',
            loadComponent: () =>
              import('./features/board/workers/workers.component').then(
                (m) => m.BoardWorkersComponent
              ),
          },
          {
            path: 'cache',
            loadComponent: () =>
              import('./features/board/cache/cache.component').then((m) => m.BoardCacheComponent),
          },
          {
            path: 'network',
            loadComponent: () =>
              import('./features/board/network/network.component').then(
                (m) => m.BoardNetworkComponent
              ),
          },
          {
            path: 'health',
            loadComponent: () =>
              import('./features/board/health/health.component').then(
                (m) => m.BoardHealthComponent
              ),
          },
          {
            path: 'expensive',
            loadComponent: () =>
              import('./features/board/expensive-jobs/expensive-jobs.component').then(
                (m) => m.BoardExpensiveJobsComponent
              ),
          },
          {
            path: 'expensive-evals',
            loadComponent: () =>
              import('./features/board/expensive-evals/expensive-evals.component').then(
                (m) => m.BoardExpensiveEvalsComponent
              ),
          },
        ],
      },

      // Organizations
      {
        path: 'organization/:org/settings',
        title: 'Organization Settings',
        resolve: { organizationAccess: organizationAccessResolver },
        loadComponent: () =>
          import('./features/organizations/organization-settings/organization-settings.component').then(
            (m) => m.OrganizationSettingsComponent
          ),
      },
      {
        path: 'organization/:org/members',
        title: 'Members & Roles',
        resolve: { organizationAccess: organizationAccessResolver },
        loadComponent: () =>
          import('./features/organizations/members-roles/members-roles.component').then(
            (m) => m.MembersRolesComponent
          ),
      },

      // Workers
      {
        path: 'organization/:org/workers',
        title: 'Workers',
        resolve: { organizationAccess: organizationAccessResolver },
        loadComponent: () =>
          import('./features/organizations/workers/workers.component').then(
            (m) => m.WorkersComponent
          ),
      },
      {
        path: 'organization/:org/workers/:workerId/metrics',
        title: 'Worker Statistics',
        resolve: { organizationAccess: organizationAccessResolver },
        loadComponent: () =>
          import('./features/organizations/workers/worker-metrics/worker-metrics.component').then(
            (m) => m.WorkerMetricsComponent
          ),
      },

      // Integrations
      {
        path: 'organization/:org/integrations',
        title: 'Integrations',
        resolve: { organizationAccess: organizationAccessResolver },
        loadComponent: () =>
          import('./features/organizations/integrations/integrations.component').then(
            (m) => m.IntegrationsComponent
          ),
      },

      // Cache Subscriptions
      {
        path: 'organization/:org/caches',
        title: 'Cache Subscriptions',
        resolve: { organizationAccess: organizationAccessResolver },
        loadComponent: () =>
          import('./features/organizations/cache-subscriptions/cache-subscriptions.component').then(
            (m) => m.CacheSubscriptionsComponent
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
      {
        path: 'settings/sessions',
        title: 'Sessions',
        loadComponent: () =>
          import('./features/settings/sessions/sessions.component').then(
            (m) => m.SessionsComponent
          ),
      },
    ],
  },

  {
    path: 'admin/github-app',
    title: 'GitHub App (admin)',
    canActivate: [authGuard, adminGuard],
    canDeactivate: [unsavedChangesGuard],
    loadComponent: () =>
      import('./features/admin/github-app/github-app.component').then(
        (m) => m.GithubAppComponent,
      ),
  },

  // Style guide (developer reference, intentionally unlinked)
  {
    path: 'styleguide',
    title: 'Style Guide',
    loadComponent: () =>
      import('./features/styleguide/styleguide.component').then((m) => m.StyleguideComponent),
  },

  // Error pages
  {
    path: 'error/500',
    title: 'Internal Server Error',
    data: { code: 500 },
    loadComponent: () =>
      import('./features/errors/error-page/error-page.component').then((m) => m.ErrorPageComponent),
  },
  {
    path: 'error/502',
    title: 'Bad Gateway',
    data: { code: 502 },
    loadComponent: () =>
      import('./features/errors/error-page/error-page.component').then((m) => m.ErrorPageComponent),
  },
  {
    path: 'error/503',
    title: 'Service Unavailable',
    data: { code: 503 },
    loadComponent: () =>
      import('./features/errors/error-page/error-page.component').then((m) => m.ErrorPageComponent),
  },
  {
    path: 'error/504',
    title: 'Gateway Timeout',
    data: { code: 504 },
    loadComponent: () =>
      import('./features/errors/error-page/error-page.component').then((m) => m.ErrorPageComponent),
  },

  // 404 fallback
  {
    path: '**',
    title: 'Page Not Found',
    data: { code: 404 },
    loadComponent: () =>
      import('./features/errors/error-page/error-page.component').then((m) => m.ErrorPageComponent),
  },
];
