/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, computed, inject, ChangeDetectionStrategy } from '@angular/core';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { IconComponent } from '@gradient/ui/ui';
import { AuthService } from '@core/services/auth.service';
import { DeniedKind } from '@core/routing/access-denied';

interface ErrorMeta {
  title: string;
  description: string;
  icon: string;
  retryable: boolean;
}

const ERROR_META: Record<number, ErrorMeta> = {
  403: {
    title: 'Admin Only',
    description: 'Instance administrators only. Ask an administrator for access.',
    icon: 'admin_panel_settings',
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

const FALLBACK_META: ErrorMeta = {
  title: 'Something Went Wrong',
  description: 'An unexpected error occurred.',
  icon: 'error',
  retryable: true,
};

const DENIED_KINDS: Record<DeniedKind, { label: string; back: string }> = {
  project: { label: 'Project', back: 'Back to Projects' },
  task: { label: 'Task', back: 'Back to Project' },
  cache: { label: 'Cache', back: 'Back to Caches' },
  team: { label: 'Team', back: 'Back to Teams' },
  evaluation: { label: 'Evaluation', back: 'Back to Project' },
  execJob: { label: 'Exec job', back: 'Back to Task' },
  worker: { label: 'Worker', back: 'Back to Workers' },
};

export interface DeniedView {
  description: string;
  backLabel: string;
  backLink: string;
}

@Component({
  selector: 'app-error-page',
  standalone: true,
  imports: [RouterModule, IconComponent],
  templateUrl: './error-page.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './error-page.component.scss',
})
export class ErrorPageComponent {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private auth = inject(AuthService);

  private query = this.route.snapshot.queryParamMap;
  protected readonly from = this.safeFrom();
  protected readonly user = this.auth.user;

  code = computed<number>(() => this.route.snapshot.data['code'] ?? 0);
  denied = computed<DeniedView | null>(() => {
    const kind = DENIED_KINDS[this.query.get('kind') as DeniedKind];
    if (this.code() !== 404 || !kind) return null;
    return {
      description: `${kind.label} "${this.query.get('name') ?? ''}" not found. It doesn't exist, or you don't have access to it.`,
      backLabel: kind.back,
      backLink: this.query.get('back') ?? '/',
    };
  });
  meta = computed<ErrorMeta>(() => {
    const base = ERROR_META[this.code()] ?? FALLBACK_META;
    return this.denied() ? { ...base, title: 'Not Found or Access Denied', icon: 'lock' } : base;
  });
  restricted = computed(() => this.code() === 403 || this.denied() !== null);

  retry(): void {
    if (this.from) {
      this.router.navigateByUrl(this.from);
    } else {
      window.location.reload();
    }
  }

  switchAccount(): void {
    this.auth.logout(this.from ?? undefined).subscribe();
  }

  private safeFrom(): string | null {
    const from = this.query.get('from');
    return from && from.startsWith('/') && !from.startsWith('/error/') ? from : null;
  }
}
