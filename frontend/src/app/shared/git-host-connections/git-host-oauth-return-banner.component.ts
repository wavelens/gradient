/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, inject, signal } from '@angular/core';
import { ActivatedRoute, Router } from '@angular/router';
import { MessageBannerComponent } from '@gradient/ui/ui';

const OAUTH_ERROR_MESSAGES: Record<string, string> = {
  denied: 'Access denied by the Git host.',
  expired: 'The approval took longer than 10 minutes or ran in another browser. Start the connection again.',
  signed_out: 'Sign-out happened before the Git host responded. Sign in again and retry.',
  forbidden: 'Missing permission to manage git host connections in this project.',
  exists: 'There is already a connection with this name in this project. Select another name and try again.',
  failed: 'Connection to the Git host failed. Try again.',
};

@Component({
  selector: 'app-git-host-oauth-return-banner',
  standalone: true,
  imports: [MessageBannerComponent],
  template: `
    @if (outcome(); as outcome) {
      <gr-message-banner [type]="outcome.severity" data-testid="oauth-outcome-banner">{{ outcome.text }}</gr-message-banner>
    }
  `,
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class GitHostOAuthReturnBannerComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private router = inject(Router);

  outcome = signal<{ severity: 'success' | 'error'; text: string } | null>(null);

  ngOnInit(): void {
    const query = this.route.snapshot.queryParamMap;
    const connected = query.get('oauth_connected');
    const error = query.get('oauth_error');
    if (!connected && !error) return;
    if (error) {
      this.outcome.set({ severity: 'error', text: OAUTH_ERROR_MESSAGES[error] ?? OAUTH_ERROR_MESSAGES['failed'] });
    } else {
      const installed = Number(query.get('webhooks_installed') ?? 0);
      const webhooks = installed > 0 ? ` Installed webhooks in ${installed} repositories.` : '';
      this.outcome.set({ severity: 'success', text: `Connected ${connected}.${webhooks}` });
    }
    void this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { oauth_connected: null, webhooks_installed: null, oauth_error: null },
      queryParamsHandling: 'merge',
      replaceUrl: true,
    });
  }
}
