/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, computed, inject } from '@angular/core';
import { ActivatedRoute, RouterModule } from '@angular/router';

interface ErrorMeta {
  title: string;
  description: string;
  icon: string;
  retryable: boolean;
}

const ERROR_META: Record<number, ErrorMeta> = {
  404: {
    title: 'Page Not Found',
    description: "The page you're looking for doesn't exist or has been moved.",
    icon: 'link_off',
    retryable: false,
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

@Component({
  selector: 'app-error-page',
  standalone: true,
  imports: [RouterModule],
  templateUrl: './error-page.component.html',
  styleUrl: './error-page.component.scss',
})
export class ErrorPageComponent {
  private route = inject(ActivatedRoute);

  code = computed<number>(() => this.route.snapshot.data['code'] ?? 0);
  meta = computed<ErrorMeta>(() => ERROR_META[this.code()] ?? FALLBACK_META);

  retry(): void {
    window.location.reload();
  }
}
