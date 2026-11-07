/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';
import { RouterLink, UrlTree } from '@angular/router';
import { MessageBannerComponent } from '@gradient/ui/ui';
import { RepositoryFetchStatus } from '@core/models';
import { relativeTime } from '@shared/text';
import { fetchFailureHint } from './fetch-failure-hint';

@Component({
  selector: 'app-repository-fetch-banner',
  standalone: true,
  imports: [MessageBannerComponent, RouterLink],
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @if (status(); as fetch) {
      @if (fetch.failed) {
        <gr-message-banner type="warning" data-testid="repository-fetch-failed">
          <strong>Gradient can't fetch the repository</strong>
          <span>Last attempt {{ since(fetch.checked_at) }}. Last success {{ since(fetch.ok_at) }}.</span>
          @if (fetch.error) {
            <code class="fetch-error">{{ fetch.error }}</code>
          }
          @if (hint(); as text) {
            <span>{{ text }}</span>
          }
          @if (canEdit()) {
            @if (fixLink(); as link) {
              <a [routerLink]="link">Open repository settings</a>
            }
          } @else {
            <span>Ask a project admin to check the repository settings.</span>
          }
        </gr-message-banner>
      }
    }
  `,
  styles: `
    :host { display: block; }
    gr-message-banner span, gr-message-banner a, .fetch-error { display: block; }
    .fetch-error { white-space: pre-wrap; word-break: break-word; }
  `,
})
export class RepositoryFetchBannerComponent {
  status = input<RepositoryFetchStatus | null | undefined>(null);
  canEdit = input(false);
  fixLink = input<UrlTree | null>(null);

  hint = computed(() => fetchFailureHint(this.status()?.error));

  since(at: string | null | undefined): string {
    return relativeTime(at);
  }
}
