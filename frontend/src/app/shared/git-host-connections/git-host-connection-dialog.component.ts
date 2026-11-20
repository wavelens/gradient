/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, inject, input, output, signal } from '@angular/core';
import {
  ButtonComponent,
  CopyFieldComponent,
  FormFieldComponent,
  DialogComponent,
  MessageBannerComponent,
  SubmitDirective,
} from '@gradient/ui/ui';
import {
  GitHostConnectionOwner,
  GitHostConnectionResponse,
  GitHostConnectionSummary,
  GitHostConnectionTestResponse,
} from '@core/models';
import { GitHostConnectionsApi } from '@core/services/git-host-connections.service';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import { MANAGED_ITEM_DISABLED_REASON } from '@shared/access/managed-disable.directive';
import { dialogFormChangesSinceOpen } from '@shared/forms';
import { gitHostSettingsLinks, webhookEvents } from '@shared/git/git-host';
import {
  GitHostConnectionFormContext,
  GitHostConnectionFormValue,
  createGitHostConnectionRequest,
  emptyGitHostConnectionForm,
  gitHostConnectionFormErrors,
  gitHostConnectionFormFromResponse,
  patchGitHostConnectionRequest,
} from './git-host-connection-form';
import { GitHostConnectionFormComponent } from './git-host-connection-form.component';

export interface GitHostConnectionDialogPrefill {
  git_host_type?: string | null;
  base_url?: string | null;
}

export interface GitHostConnectionEditOptions {
  focusWebhookSecret?: boolean;
}

interface SavedConnectionOutcome {
  connection: GitHostConnectionResponse;
  tokenCheck: GitHostConnectionTestResponse | 'running' | null;
  newWebhookSecret: string | null;
}

@Component({
  selector: 'app-git-host-connection-dialog',
  standalone: true,
  imports: [ButtonComponent, CopyFieldComponent, DialogComponent, FormFieldComponent, MessageBannerComponent, SubmitDirective, GitHostConnectionFormComponent],
  template: `
    <gr-dialog #dialog [header]="header()" [visible]="open()" (visibleChange)="setOpen($event)" [error]="error()" [busy]="saving()"
      [confirmDiscard]="!outcome() && formChanges.changed()" [closable]="!webhookSecret()" width="560px">
      @if (outcome(); as outcome) {
        @switch (tokenCheckState()) {
          @case ('running') {
            <gr-message-banner type="info" data-testid="token-check">Checking access token...</gr-message-banner>
          }
          @case ('success') {
            <gr-message-banner type="success" data-testid="token-check">Signed in as {{ tokenCheckAccount() }}</gr-message-banner>
          }
          @case ('failure') {
            <gr-message-banner type="error" data-testid="token-check">{{ tokenCheckError() }}</gr-message-banner>
          }
        }
        @if (showsWebhookSetup()) {
          <div class="connection-dialog-webhook" data-testid="webhook-setup">
            @if (outcome.connection.webhook_url; as webhookUrl) {
              <gr-form-field label="Webhook URL"><gr-copy-field [value]="webhookUrl" /></gr-form-field>
            }
            @if (webhookSecret(); as secret) {
              <gr-form-field label="Webhook secret"><gr-copy-field [value]="secret" data-testid="webhook-secret" /></gr-form-field>
            }
            <p class="text-secondary">Events: {{ events() }}</p>
            @if (webhookSettingsUrl(); as href) {
              <a grButton severity="secondary" icon="open_in_new" label="Open webhook settings" [href]="href" target="_blank"
                rel="noopener noreferrer"></a>
            }
          </div>
        }
      } @else {
        @if (editing()?.managed) {
          <gr-message-banner type="info">{{ managedReason }}</gr-message-banner>
        }
        @if (open()) {
          <app-git-host-connection-form [(value)]="form" [context]="context()" [showErrors]="attempted()" [oauthProject]="oauthProject()"
            [focusWebhookSecret]="focusWebhookSecret()" />
        }
      }
      <div grDialogFooter>
        @if (outcome()) {
          <button grButton label="Done" icon="check" (click)="setOpen(false)" data-testid="connection-done"></button>
        } @else {
          <button grButton label="Cancel" severity="secondary" (click)="dialog.close()" [disabled]="saving()"></button>
          @if (!editing()?.managed) {
            <button grButton [label]="editing() ? 'Save' : 'Create'" icon="check" (grSubmit)="save()" [loading]="saving()"
              [disabled]="saving()" data-testid="save-connection"></button>
          }
        }
      </div>
    </gr-dialog>
  `,
  styles: `
    @use '@gradient/ui/styles/variables' as *;

    .connection-dialog-webhook { display: flex; flex-direction: column; gap: $spacing-md; align-items: stretch; }
    .connection-dialog-webhook > a { align-self: flex-start; }
  `,
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class GitHostConnectionDialogComponent {
  private api = inject(GitHostConnectionsApi);
  private notify = inject(NotifyService);
  protected readonly managedReason = MANAGED_ITEM_DISABLED_REASON;

  owner = input.required<GitHostConnectionOwner>();
  existingNames = input<readonly string[]>([]);
  repository = input<string | null>(null);
  offersOAuthSignIn = input(false);
  saved = output<GitHostConnectionResponse>();

  open = signal(false);
  editing = signal<GitHostConnectionResponse | null>(null);
  openingId = signal<string | null>(null);
  focusWebhookSecret = signal(false);
  form = signal<GitHostConnectionFormValue>(emptyGitHostConnectionForm());
  attempted = signal(false);
  saving = signal(false);
  error = signal<string | null>(null);
  outcome = signal<SavedConnectionOutcome | null>(null);
  readonly formChanges = dialogFormChangesSinceOpen(() => this.form());

  context = computed<GitHostConnectionFormContext>(() => ({ editing: this.editing(), existingNames: this.existingNames() }));
  oauthProject = computed(() => {
    const owner = this.owner();
    return this.offersOAuthSignIn() && owner.type === 'project' ? owner.name : null;
  });
  header = computed(() => {
    if (this.outcome()) return 'Git host connection saved';
    return this.editing() ? 'Edit Git host connection' : 'New git host connection';
  });

  tokenCheckState = computed(() => {
    const check = this.outcome()?.tokenCheck;
    if (!check) return null;
    if (check === 'running') return 'running';
    return check.success ? 'success' : 'failure';
  });
  tokenCheckAccount = computed(() => {
    const check = this.outcome()?.tokenCheck;
    return check && check !== 'running' ? (check.account ?? 'unknown account') : '';
  });
  tokenCheckError = computed(() => {
    const check = this.outcome()?.tokenCheck;
    return check && check !== 'running' ? `Access token doesn't work: ${check.error ?? 'the Git host refused it.'}` : '';
  });
  showsWebhookSetup = computed(() => {
    const outcome = this.outcome();
    if (!outcome?.connection.receives_events) return false;
    return !!outcome.newWebhookSecret || !outcome.connection.has_access_token || this.tokenCheckState() === 'failure';
  });
  webhookSecret = computed(() => {
    const outcome = this.outcome();
    return this.showsWebhookSetup() ? (outcome?.newWebhookSecret ?? outcome?.connection.generated_secret ?? null) : null;
  });
  events = computed(() => webhookEvents(this.outcome()?.connection.git_host_type ?? ''));
  webhookSettingsUrl = computed(() => {
    const repository = this.repository();
    const connection = this.outcome()?.connection;
    return repository && connection ? (gitHostSettingsLinks(repository, connection.git_host_type)?.webhooks ?? null) : null;
  });

  openCreate(prefill: GitHostConnectionDialogPrefill = {}): void {
    this.editing.set(null);
    this.focusWebhookSecret.set(false);
    this.form.set(emptyGitHostConnectionForm(prefill));
    this.show();
  }

  openEdit(connection: GitHostConnectionSummary, options: GitHostConnectionEditOptions = {}): void {
    if (this.openingId()) return;
    this.openingId.set(connection.id);
    this.api.get(this.owner(), connection.id).subscribe({
      next: (full) => {
        this.openingId.set(null);
        this.editing.set(full);
        this.focusWebhookSecret.set(!!options.focusWebhookSecret);
        this.form.set(gitHostConnectionFormFromResponse(full));
        this.show();
      },
      error: (error: unknown) => {
        this.openingId.set(null);
        this.notify.failure('Failed to load connection to Git host', error);
      },
    });
  }

  setOpen(open: boolean): void {
    this.open.set(open);
    if (!open) this.outcome.set(null);
  }

  save(): void {
    if (this.saving()) return;
    this.attempted.set(true);
    const target = this.editing();
    if (target?.managed || Object.keys(gitHostConnectionFormErrors(this.form(), this.context())).length > 0) return;
    const tokenChanged = !!this.form().access_token.trim();
    const sentSecret = this.form().secret.trim();
    const request = target
      ? this.api.patch(this.owner(), target.id, patchGitHostConnectionRequest(this.form(), target, this.context()))
      : this.api.create(this.owner(), createGitHostConnectionRequest(this.form(), this.context()));
    this.saving.set(true);
    this.error.set(null);
    request.subscribe({
      next: (connection) => {
        this.saving.set(false);
        this.saved.emit(connection);
        const newWebhookSecret = target ? sentSecret || connection.generated_secret || null : null;
        if (target && !tokenChanged && !newWebhookSecret) {
          this.notify.success('Git host connection saved');
          this.setOpen(false);
          return;
        }
        this.showOutcome(connection, tokenChanged, newWebhookSecret);
      },
      error: (error: unknown) => {
        this.saving.set(false);
        this.error.set(apiErrorMessage(error, 'Failed to save the connection to the Git host'));
      },
    });
  }

  private showOutcome(connection: GitHostConnectionResponse, checkToken: boolean, newWebhookSecret: string | null): void {
    this.outcome.set({ connection, tokenCheck: checkToken ? 'running' : null, newWebhookSecret });
    if (!checkToken) return;
    this.api.test(this.owner(), connection.id).subscribe({
      next: (result) => this.setTokenCheck(connection, result),
      error: (error: unknown) => this.setTokenCheck(connection, { success: false, error: apiErrorMessage(error, 'the check failed.') }),
    });
  }

  private setTokenCheck(connection: GitHostConnectionResponse, tokenCheck: GitHostConnectionTestResponse): void {
    const outcome = this.outcome();
    if (outcome?.connection.id === connection.id) this.outcome.set({ ...outcome, connection, tokenCheck });
  }

  private show(): void {
    this.attempted.set(false);
    this.error.set(null);
    this.outcome.set(null);
    this.formChanges.mark();
    this.open.set(true);
  }
}
