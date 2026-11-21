/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, inject, input, output, signal } from '@angular/core';
import { ActivatedRoute, Router } from '@angular/router';
import { MessageBannerComponent } from '@gradient/ui/ui';
import { CompleteGitHostOAuthConnectionRequest, CompletedGitHostOAuthConnection } from '@core/models';
import { ApiService, apiErrorMessage } from '@core/services/api.service';
import { gitHostOauthUnknownFailureText, oauthErrorTextForValue } from './git-host-oauth-error-messages';

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
  private api = inject(ApiService);

  projectName = input.required<string>();
  gitHostConnectionCreated = output<CompletedGitHostOAuthConnection>();

  outcome = signal<{ severity: 'success' | 'error'; text: string } | null>(null);

  ngOnInit(): void {
    const query = this.route.snapshot.queryParamMap;
    const code = query.get('code');
    const state = query.get('state');
    const error = query.get('oauth_error');
    if (!error && !(code && state)) return;
    if (error) {
      this.outcome.set({ severity: 'error', text: oauthErrorTextForValue(error) ?? gitHostOauthUnknownFailureText });
    } else if (code && state) {
      this.finishForgejoOAuthConnectionAndShowBanner({ code, state });
    }
    void this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { code: null, state: null, oauth_error: null },
      queryParamsHandling: 'merge',
      replaceUrl: true,
    });
  }

  private finishForgejoOAuthConnectionAndShowBanner(body: CompleteGitHostOAuthConnectionRequest): void {
    this.api
      .post<CompletedGitHostOAuthConnection>(`projects/${encodeURIComponent(this.projectName())}/git-host-connections/oauth/complete`, body)
      .subscribe({
        next: (connection) => {
          const { name, webhooks_installed } = connection;
          const webhooks = webhooks_installed > 0 ? `Installed webhooks in ${webhooks_installed} repositories.` : '';
          const text = [`Connected ${name}.`, webhooks].filter(Boolean).join(' ');
          this.outcome.set({ severity: 'success', text });
          this.gitHostConnectionCreated.emit(connection);
        },
        error: (failure: unknown) => this.outcome.set({ severity: 'error', text: apiErrorMessage(failure, gitHostOauthUnknownFailureText) }),
      });
  }
}
