/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, computed, inject, ChangeDetectionStrategy } from '@angular/core';
import { Location } from '@angular/common';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { ButtonComponent, IconComponent } from '@gradient/ui/ui';
import { AuthService } from '@core/services/auth.service';
import { PendingUserDecisionsService } from '@core/services/pending-invites.service';
import { DeniedKind } from '@core/routing/access-denied';
import { getInAppReturnPathIfNotErrorPage, links } from '@shared/routing';

interface ErrorMeta {
  title: string;
  description: string;
  icon: string;
  retryable: boolean;
}

const ERROR_META: Record<number, ErrorMeta> = {
  403: {
    title: 'Access Denied',
    description: "You don't have access to this page.",
    icon: 'lock',
    retryable: false,
  },
  404: {
    title: 'Page Not Found',
    description: "The page you're looking for doesn't exist or has been moved.",
    icon: 'link_off',
    retryable: false,
  },
  429: {
    title: 'Too Many Requests',
    description: 'The server is limiting how often it answers this address. Please try again in a few seconds.',
    icon: 'hourglass_top',
    retryable: true,
  },
  500: {
    title: 'Internal Server Error',
    description: 'Something went wrong on the server. Please try again later.',
    icon: 'bug_report',
    retryable: true,
  },
  502: {
    title: 'Bad Gateway',
    description: 'The server received an invalid response from an upstream service.',
    icon: 'cloud_off',
    retryable: true,
  },
  503: {
    title: 'Service Unavailable',
    description: 'The server is temporarily unavailable. Please try again in a moment.',
    icon: 'dns',
    retryable: true,
  },
  504: {
    title: 'Gateway Timeout',
    description: 'The server took too long to respond. Check your connection and try again.',
    icon: 'timer_off',
    retryable: true,
  },
};

const ADMIN_ONLY_META: ErrorMeta = {
  title: 'Admin Only',
  description: 'Instance administrators only. Ask an administrator for access.',
  icon: 'admin_panel_settings',
  retryable: false,
};

const FALLBACK_META: ErrorMeta = {
  title: 'Something Went Wrong',
  description: 'An unexpected error occurred.',
  icon: 'error',
  retryable: true,
};

const DENIED_KINDS: Record<DeniedKind, { label: string; back: string }> = {
  project: { label: 'Project', back: 'Back to Projects' },
  task: { label: 'Task', back: 'Back to Projects' },
  cache: { label: 'Cache', back: 'Back to Caches' },
  team: { label: 'Team', back: 'Back to Teams' },
  evaluation: { label: 'Evaluation', back: 'Back to Projects' },
  build: { label: 'Build', back: 'Back to Projects' },
  execJob: { label: 'Exec job', back: 'Back to Projects' },
  job: { label: 'Job', back: 'Back to Job Board' },
  clusterJob: { label: 'Cluster job', back: 'Back to Job Board' },
  worker: { label: 'Worker', back: 'Back to Workers' },
};

const PROJECT_PAGE = /^\/project\/[^/?#]+$/;

export interface DeniedView {
  description: string;
  backLabel: string;
  backLink: string;
}

function deniedDescription(notFound: string, signedIn: boolean): string {
  return signedIn ? `${notFound} It doesn't exist, or you don't have access to it.` : `${notFound} It doesn't exist, or it's private and needs sign-in.`;
}

@Component({
  selector: 'app-error-page',
  standalone: true,
  imports: [RouterModule, ButtonComponent, IconComponent],
  templateUrl: './error-page.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './error-page.component.scss',
})
export class ErrorPageComponent {
  protected readonly links = links;
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private location = inject(Location);
  private readonly cameFromApp = !!this.router.currentNavigation()?.previousNavigation;
  private auth = inject(AuthService);
  protected pending = inject(PendingUserDecisionsService);

  private query = this.route.snapshot.queryParamMap;
  protected readonly from = this.safeFrom();
  protected readonly user = this.auth.user;

  code = computed<number>(() => this.route.snapshot.data['code'] ?? 0);
  denied = computed<DeniedView | null>(() => {
    const kind = DENIED_KINDS[this.query.get('kind') as DeniedKind];
    if (this.code() !== 404 || !kind) return null;
    const back = this.query.get('back') ?? '/';
    const name = this.query.get('name');
    return {
      description: deniedDescription(name ? `${kind.label} "${name}" not found.` : `${kind.label} not found.`, !!this.user()),
      backLabel: PROJECT_PAGE.test(back) ? 'Back to Project' : kind.back,
      backLink: back,
    };
  });
  meta = computed<ErrorMeta>(() => {
    if (this.code() === 403) {
      if (this.query.get('only') === 'admin') return ADMIN_ONLY_META;
      return this.user() ? ERROR_META[403] : { ...ERROR_META[403], description: 'Sign in to open this page.' };
    }
    const base = ERROR_META[this.code()] ?? FALLBACK_META;
    return this.denied() ? { ...base, title: 'Not Found or Access Denied', icon: 'lock' } : base;
  });
  restricted = computed(() => this.code() === 403 || this.denied() !== null);
  home = computed(() =>
    this.code() === 403 && this.from?.startsWith('/board/')
      ? { label: 'Back to Job Board', link: links.board() }
      : { label: 'Go Home', link: links.dashboard() },
  );

  constructor() {
    if (this.denied() && this.user()) this.pending.refresh();
  }

  retry(): void {
    if (this.from) void this.router.navigateByUrl(this.from);
    else if (this.cameFromApp) this.location.back();
    else void this.router.navigateByUrl('/');
  }

  switchAccount(): void {
    this.auth.logout(this.from ?? undefined).subscribe();
  }

  private safeFrom(): string | null {
    return getInAppReturnPathIfNotErrorPage(this.query.get('from'));
  }
}
