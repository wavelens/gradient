/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, inject, input } from '@angular/core';
import { ButtonComponent } from '@gradient/ui/ui';
import { environment } from '@environments/environment';
import { ConfigService } from '@core/services/config.service';
import { gitHostLabel } from '@shared/git/git-host';
import { gitHostConnectionNameFromHost } from './git-host-connection-form';

@Component({
  selector: 'app-git-host-oauth-sign-in',
  standalone: true,
  imports: [ButtonComponent],
  template: `
    @if (oauthApp) {
      <button grButton type="button" severity="secondary" icon="login" [label]="'Sign in with ' + hostLabel"
        (click)="signIn()" data-testid="oauth-sign-in"></button>
    }
  `,
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class GitHostOAuthSignInComponent {
  readonly oauthApp = inject(ConfigService).giteaOauthAppConfig;
  readonly hostLabel = this.oauthApp ? `${gitHostLabel(this.oauthApp.git_host_type)} (${new URL(this.oauthApp.instance_base_url).host})` : '';

  projectName = input.required<string>();
  existingNames = input.required<readonly string[]>();

  connectUrl = computed(() => {
    const name = gitHostConnectionNameFromHost(this.oauthApp?.instance_base_url ?? '', this.existingNames());
    const query = new URLSearchParams({ name });
    return `${environment.apiUrl}/projects/${encodeURIComponent(this.projectName())}/git-host-connections/oauth/connect?${query}`;
  });

  signIn(): void {
    window.location.href = this.connectUrl();
  }
}
