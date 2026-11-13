/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, input, signal } from '@angular/core';
import { ActivatedRoute, Router } from '@angular/router';
import { FormsModule } from '@angular/forms';
import {
  ButtonComponent,
  DialogComponent,
  FormFieldComponent,
  InputDirective,
  MessageBannerComponent,
  SettingsSectionComponent,
  SubmitDirective,
} from '@gradient/ui/ui';
import { environment } from '@environments/environment';
import { AccessState } from '@core/models';
import { ConfigService } from '@core/services/config.service';
import { gitHostLabel } from '@shared/git/git-host';
import { slugify } from '@shared/text';

const NAME_PATTERN = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/;

const OAUTH_ERROR_MESSAGES: Record<string, string> = {
  denied: 'Access denied by the Git host.',
  signed_out: 'Sign-out happened before the Git host responded. Sign in again and retry.',
  forbidden: 'Missing permission to manage connections for Git hosts in this project.',
  exists: 'There is already a connection with this name in this project. Select another name and try again.',
  failed: 'Connection to the Git host failed. Try again.',
};

function deriveDefaultConnectionNameFromUrl(url: string): string {
  const labels = new URL(url).hostname.split('.');
  return slugify((labels.length > 1 ? labels.slice(0, -1) : labels).join('-'));
}

@Component({
  selector: 'app-forgejo-oauth-connection',
  standalone: true,
  imports: [
    FormsModule,
    ButtonComponent,
    DialogComponent,
    FormFieldComponent,
    InputDirective,
    MessageBannerComponent,
    SettingsSectionComponent,
    SubmitDirective,
  ],
  template: `
    @if (gitHostOauthApprovalReturnBannerSignal(); as outcome) {
      <gr-message-banner [type]="outcome.severity" data-testid="oauth-outcome-banner">{{ outcome.text }}</gr-message-banner>
    }
    @if (giteaForgejoOAuth2AppConfig && access().canEdit && !access().managed) {
      <gr-settings-section [title]="oauth2AppGitHostTypeAndInstanceHostnameLabel" data-testid="oauth-connect-section">
        <p class="text-secondary">Connect this project to a Git host with OAuth2. Gradient can create a connection after authorization on the Git host. That connection can receive events and report commit status. Gradient can refresh the access token automatically. Gradient can also install Webhooks on repositories of project tasks that don't have a connection on this Git host yet.</p>
        <button grButton [label]="'Connect with ' + oauth2AppGitHostTypeAndInstanceHostnameLabel" icon="link"
          (click)="openOAuth2ConnectDialogWithDefaultNameFromHostname()"></button>
      </gr-settings-section>

      <gr-dialog [header]="'Connect with ' + oauth2AppGitHostTypeAndInstanceHostnameLabel" [visible]="isOAuth2ConnectionNameDialogOpen()"
        (visibleChange)="isOAuth2ConnectionNameDialogOpen.set($event)" width="520px">
        <gr-message-banner type="warning">Gradient can redirect to the Git host for approval. Access tokens for the approving account have full access, so approve with a bot account.</gr-message-banner>
        <gr-form-field label="Name" for="oauth-name" required [invalid]="submitAttempted() && !!connectionNameValidationError()"
          [error]="connectionNameValidationError() ?? ''" hint="Name of the connection. Lowercase letters, numbers, dashes.">
          <input grInput id="oauth-name" [ngModel]="name()" (ngModelChange)="name.set($event)" />
        </gr-form-field>
        <gr-form-field label="Display name" for="oauth-display-name" hint="Human-readable label in the connection list.">
          <input grInput id="oauth-display-name" [ngModel]="displayName()" (ngModelChange)="displayName.set($event)" placeholder="Defaults to the name" />
        </gr-form-field>
        <div grDialogFooter>
          <button grButton label="Cancel" severity="secondary" (click)="isOAuth2ConnectionNameDialogOpen.set(false)"></button>
          <button grButton label="Continue" icon="open_in_new" (grSubmit)="validateNameAndNavigateToConnectUrl()" data-testid="oauth-continue"></button>
        </div>
      </gr-dialog>
    }
  `,
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class ForgejoOAuthConnectionComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private router = inject(Router);

  projectName = input.required<string>();
  access = input.required<AccessState>();
  existingConnectionNames = input.required<string[]>();

  readonly giteaForgejoOAuth2AppConfig = inject(ConfigService).giteaOauthAppConfig;
  readonly oauth2AppGitHostTypeAndInstanceHostnameLabel = this.giteaForgejoOAuth2AppConfig
    ? `${gitHostLabel(this.giteaForgejoOAuth2AppConfig.git_host_type)} (${new URL(this.giteaForgejoOAuth2AppConfig.instance_base_url).host})`
    : '';

  gitHostOauthApprovalReturnBannerSignal = signal<{ severity: 'success' | 'error'; text: string } | null>(null);
  isOAuth2ConnectionNameDialogOpen = signal(false);
  submitAttempted = signal(false);
  name = signal('');
  displayName = signal('');

  connectionNameValidationError = computed(() => {
    const name = this.name().trim();
    if (!name) return 'Enter a name.';
    if (!NAME_PATTERN.test(name)) return "Use lowercase letters, digits and dashes. Don't start or end with a dash.";
    return this.existingConnectionNames().includes(name) ? 'This owner already has a connection with this name.' : null;
  });

  oauth2ConnectUrlWithNameAndDisplayName = computed(() => {
    const query = new URLSearchParams({ name: this.name().trim() });
    const displayName = this.displayName().trim();
    if (displayName) query.set('display_name', displayName);
    return `${environment.apiUrl}/projects/${encodeURIComponent(this.projectName())}/git-host-connections/oauth/connect?${query}`;
  });

  ngOnInit(): void {
    this.readConnectOutcomeFromQueryParams();
  }

  private readConnectOutcomeFromQueryParams(): void {
    const query = this.route.snapshot.queryParamMap;
    const connected = query.get('oauth_connected');
    const error = query.get('oauth_error');
    if (!connected && !error) return;
    if (error) {
      this.gitHostOauthApprovalReturnBannerSignal.set({ severity: 'error', text: OAUTH_ERROR_MESSAGES[error] ?? OAUTH_ERROR_MESSAGES['failed'] });
    } else {
      const installed = Number(query.get('webhooks_installed') ?? 0);
      const webhooks = installed > 0 ? ` Installed webhooks in ${installed} repositories.` : '';
      this.gitHostOauthApprovalReturnBannerSignal.set({ severity: 'success', text: `Connected ${connected}.${webhooks}` });
    }
    void this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { oauth_connected: null, webhooks_installed: null, oauth_error: null },
      queryParamsHandling: 'merge',
      replaceUrl: true,
    });
  }

  openOAuth2ConnectDialogWithDefaultNameFromHostname(): void {
    const app = this.giteaForgejoOAuth2AppConfig;
    if (!app) return;
    this.name.set(deriveDefaultConnectionNameFromUrl(app.instance_base_url));
    this.displayName.set('');
    this.submitAttempted.set(false);
    this.isOAuth2ConnectionNameDialogOpen.set(true);
  }

  validateNameAndNavigateToConnectUrl(): void {
    this.submitAttempted.set(true);
    if (this.connectionNameValidationError()) return;
    window.location.href = this.oauth2ConnectUrlWithNameAndDisplayName();
  }
}
