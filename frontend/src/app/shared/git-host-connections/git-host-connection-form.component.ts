/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, booleanAttribute, computed, inject, input, linkedSignal, model, untracked } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { BadgeComponent, ButtonComponent, CheckboxComponent, DividerComponent, FormFieldComponent, InputDirective, SelectComponent } from '@gradient/ui/ui';
import { ConfigService } from '@core/services/config.service';
import { SELF_HOSTED_GIT_HOSTS, knownGitHostType, repositoryHost } from '@shared/git/git-host';
import {
  GitHostConnectionFormContext,
  GitHostConnectionFormValue,
  effectiveConnectionName,
  effectiveReportsStatus,
  gitHostBaseUrl,
  gitHostConnectionFormErrors,
  hasNonDefaultAdvancedSettings,
} from './git-host-connection-form';
import { GitHostOAuthSignInComponent } from './git-host-oauth-sign-in.component';

@Component({
  selector: 'app-git-host-connection-form',
  standalone: true,
  imports: [FormsModule, BadgeComponent, ButtonComponent, CheckboxComponent, DividerComponent, FormFieldComponent, InputDirective, SelectComponent, GitHostOAuthSignInComponent],
  template: `
    @if (offersOAuthSignIn()) {
      <app-git-host-oauth-sign-in [projectName]="oauthProject()!" [existingNames]="context().existingNames" />
      <gr-divider />
    }
    <gr-form-field label="Host type" for="connection-host-type">
      <gr-select inputId="connection-host-type" [options]="gitHostOptions" optionLabel="label" optionValue="value"
        [ngModel]="value().git_host_type" (ngModelChange)="patch({ git_host_type: $event })" />
    </gr-form-field>
    <gr-form-field label="Host URL" for="connection-base-url" required [invalid]="!!shownErrors().base_url" [error]="shownErrors().base_url ?? ''">
      <input grInput id="connection-base-url" [ngModel]="value().base_url" (ngModelChange)="patch({ base_url: $event })"
        (paste)="takeHostFromPastedUrl($event)" (blur)="normalizeHostUrl()" placeholder="https://git.example.com" />
    </gr-form-field>
    <gr-form-field label="Access token" for="connection-access-token" [required]="tokenRequired()"
      [invalid]="!!shownErrors().access_token" [error]="shownErrors().access_token ?? ''">
      <input grInput id="connection-access-token" type="password" autocomplete="off" [ngModel]="value().access_token"
        (ngModelChange)="patch({ access_token: $event })" [placeholder]="context().editing?.has_access_token ? 'Unchanged' : ''" />
      @if (context().editing?.access_token_refreshes_automatically) {
        <gr-badge data-testid="token-refreshes-badge">Automatic renewal</gr-badge>
      }
    </gr-form-field>
    <gr-form-field label="Name" for="connection-name" required [invalid]="!!shownErrors().name" [error]="shownErrors().name ?? ''">
      <input grInput id="connection-name" [ngModel]="name()" (ngModelChange)="patch({ name: $event })" />
    </gr-form-field>

    <button grButton type="button" severity="secondary" size="small" label="Advanced" [icon]="advancedOpen() ? 'expand_less' : 'expand_more'"
      [attr.aria-expanded]="advancedOpen()" aria-controls="connection-advanced" (click)="advancedOpen.set(!advancedOpen())"
      data-testid="connection-advanced-toggle"></button>
    @if (advancedOpen()) {
      <div id="connection-advanced" class="connection-form-advanced">
        <gr-form-field label="Display name" for="connection-display-name">
          <input grInput id="connection-display-name" [ngModel]="value().display_name" (ngModelChange)="patch({ display_name: $event })"
            [placeholder]="name()" />
        </gr-form-field>
        <gr-form-field [invalid]="!!shownErrors().capabilities" [error]="shownErrors().capabilities ?? ''">
          <gr-checkbox inputId="connection-receives-events" label="Receives events" [ngModel]="value().receives_events"
            (ngModelChange)="patch({ receives_events: $event })" />
        </gr-form-field>
        <gr-form-field>
          <gr-checkbox inputId="connection-reports-status" label="Reports status" [ngModel]="reportsStatus()"
            (ngModelChange)="patch({ reports_status: $event })" />
        </gr-form-field>
        @if (value().receives_events) {
          <gr-form-field label="Webhook secret" for="connection-secret" [invalid]="!!shownErrors().secret" [error]="shownErrors().secret ?? ''">
            <div class="connection-form-secret">
              <input grInput id="connection-secret" autocomplete="off" [attr.cdkFocusInitial]="focusWebhookSecret() ? '' : null" [ngModel]="value().secret" (ngModelChange)="patch({ secret: $event })"
                [placeholder]="context().editing?.has_secret ? 'Unchanged' : 'Generated on save'" />
              <button grButton type="button" icon="refresh" severity="secondary" aria-label="Generate webhook secret" (click)="generateSecret()"></button>
            </div>
          </gr-form-field>
          <gr-form-field label="Allowed source IPs" for="connection-allowed-ips" [invalid]="!!shownErrors().allowed_ips" [error]="shownErrors().allowed_ips ?? ''">
            <textarea grInput id="connection-allowed-ips" rows="3" [ngModel]="value().allowed_ips" (ngModelChange)="patch({ allowed_ips: $event })"
              placeholder="Any address"></textarea>
          </gr-form-field>
        }
      </div>
    }
  `,
  styles: `
    @use '@gradient/ui/styles/variables' as *;

    :host { display: flex; flex-direction: column; gap: $spacing-md; align-items: stretch; }
    :host > button { align-self: flex-start; }
    .connection-form-advanced { display: flex; flex-direction: column; gap: $spacing-md; }
    .connection-form-secret { display: flex; gap: $spacing-sm; }
    .connection-form-secret input { flex: 1; min-width: 0; }
  `,
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class GitHostConnectionFormComponent {
  private oauthApp = inject(ConfigService).giteaOauthAppConfig;
  protected readonly gitHostOptions = SELF_HOSTED_GIT_HOSTS;

  value = model.required<GitHostConnectionFormValue>();
  context = input.required<GitHostConnectionFormContext>();
  showErrors = input(false, { transform: booleanAttribute });
  oauthProject = input<string | null>(null);
  focusWebhookSecret = input(false, { transform: booleanAttribute });

  name = computed(() => effectiveConnectionName(this.value(), this.context()));
  reportsStatus = computed(() => effectiveReportsStatus(this.value(), this.context()));
  tokenRequired = computed(() => this.reportsStatus() && !this.context().editing?.has_access_token);
  errors = computed(() => gitHostConnectionFormErrors(this.value(), this.context()));
  shownErrors = computed(() => (this.showErrors() ? this.errors() : {}));
  hasErrors = computed(() => Object.keys(this.errors()).length > 0);
  private advancedHasShownErrors = computed(() => {
    const errors = this.shownErrors();
    return !!(errors.capabilities || errors.secret || errors.allowed_ips);
  });
  advancedOpen = linkedSignal<boolean, boolean>({
    source: this.advancedHasShownErrors,
    computation: (hasErrors, previous) =>
      hasErrors || (previous ? previous.value : untracked(() => this.focusWebhookSecret() || hasNonDefaultAdvancedSettings(this.value(), this.context()))),
  });

  offersOAuthSignIn = computed(() => {
    if (!this.oauthApp || !this.oauthProject() || this.context().editing) return false;
    const host = repositoryHost(this.value().base_url);
    return !host || host === new URL(this.oauthApp.instance_base_url).host.toLowerCase();
  });

  patch(change: Partial<GitHostConnectionFormValue>): void {
    this.value.update((value) => ({ ...value, ...change }));
  }

  takeHostFromPastedUrl(event: ClipboardEvent): void {
    const text = event.clipboardData?.getData('text') ?? '';
    if (!repositoryHost(text)) return;
    event.preventDefault();
    this.applyHostUrl(text);
  }

  normalizeHostUrl(): void {
    if (this.value().base_url.trim()) this.applyHostUrl(this.value().base_url);
  }

  private applyHostUrl(text: string): void {
    const base_url = gitHostBaseUrl(text);
    this.patch({ base_url, git_host_type: knownGitHostType(base_url) ?? this.value().git_host_type });
  }

  generateSecret(): void {
    const bytes = new Uint8Array(32);
    crypto.getRandomValues(bytes);
    this.patch({ secret: Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('') });
  }
}
