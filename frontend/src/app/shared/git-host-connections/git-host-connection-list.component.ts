/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, input, signal } from '@angular/core';
import { NgTemplateOutlet } from '@angular/common';
import { FormsModule } from '@angular/forms';
import {
  BadgeComponent,
  ButtonComponent,
  CheckboxComponent,
  CopyFieldComponent,
  DialogComponent,
  EmptyStateComponent,
  FormFieldComponent,
  InputDirective,
  LoadStateComponent,
  MessageBannerComponent,
  SelectComponent,
  SettingsSectionComponent,
  SubmitDirective,
} from '@gradient/ui/ui';
import {
  AccessState,
  CreateGitHostConnectionRequest,
  GitHostConnectionOwner,
  GitHostConnectionResponse,
  GitHostConnectionSummary,
  GitHostType,
  PatchGitHostConnectionRequest,
  getGitHostConnectionOwnerReadableText,
  isSameGitHostConnectionOwner,
} from '@core/models';
import { GitHostConnectionsApi } from '@core/services/git-host-connections.service';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import { ConfirmService } from '@shared/confirm';
import { MANAGED_ITEM_DISABLED_REASON, ManagedDisableDirective } from '@shared/access/managed-disable.directive';
import { IncomingDeliveriesComponent } from '@shared/git/incoming-deliveries.component';
import { ForgejoOAuthConnectionComponent } from './forgejo-oauth-connection.component';
import { SELF_HOSTED_GIT_HOSTS, gitHostLabel, webhookEvents } from '@shared/git/git-host';
import { dialogFormChangesSinceOpen, fieldChecks, isIpNetwork, splitLines } from '@shared/forms';
import { formatEntityLabel } from '@shared/ui';

interface ConnectionForm {
  name: string;
  display_name: string;
  git_host_type: GitHostType;
  base_url: string;
  access_token: string;
  secret: string;
  allowed_ips: string;
  receives_events: boolean;
  reports_status: boolean;
}

const NAME_PATTERN = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/;

function emptyForm(): ConnectionForm {
  return {
    name: '',
    display_name: '',
    git_host_type: 'forgejo',
    base_url: '',
    access_token: '',
    secret: '',
    allowed_ips: '',
    receives_events: true,
    reports_status: false,
  };
}

@Component({
  selector: 'app-git-host-connection-list',
  standalone: true,
  imports: [
    FormsModule,
    NgTemplateOutlet,
    BadgeComponent,
    ButtonComponent,
    CheckboxComponent,
    CopyFieldComponent,
    DialogComponent,
    EmptyStateComponent,
    FormFieldComponent,
    InputDirective,
    LoadStateComponent,
    MessageBannerComponent,
    SelectComponent,
    SettingsSectionComponent,
    SubmitDirective,
    ManagedDisableDirective,
    IncomingDeliveriesComponent,
    ForgejoOAuthConnectionComponent,
  ],
  templateUrl: './git-host-connection-list.component.html',
  styleUrl: './git-host-connection-list.component.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class GitHostConnectionListComponent implements OnInit {
  private api = inject(GitHostConnectionsApi);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);

  gitHostConnectionOwner = input.required<GitHostConnectionOwner>();
  access = input.required<AccessState>();

  protected readonly gitHostOptions = SELF_HOSTED_GIT_HOSTS;
  protected readonly gitHostLabel = gitHostLabel;
  protected readonly ownerText = getGitHostConnectionOwnerReadableText;
  protected readonly managedReason = MANAGED_ITEM_DISABLED_REASON;

  connections = signal<GitHostConnectionSummary[]>([]);
  loading = signal(true);
  loadError = signal<unknown>(null);

  ownConnections = computed(() =>
    this.connections().filter((c) => isSameGitHostConnectionOwner(c.owner, this.gitHostConnectionOwner())),
  );
  ownConnectionNames = computed(() => this.ownConnections().map((c) => c.name));
  inheritedReadOnlyConnections = computed(() =>
    this.connections().filter((c) => !isSameGitHostConnectionOwner(c.owner, this.gitHostConnectionOwner())),
  );
  canCreate = computed(() => this.access().canEdit);

  dialogOpen = signal(false);
  editing = signal<GitHostConnectionResponse | null>(null);
  openingId = signal<string | null>(null);
  saving = signal(false);
  dialogError = signal<string | null>(null);
  form = signal<ConnectionForm>(emptyForm());
  readonly formChanges = dialogFormChangesSinceOpen(() => this.form());

  createdConnectionWithOneTimeSecret = signal<{ webhookUrl: string; secret: string; gitHostType: string } | null>(null);
  deliveriesShownFor = signal<string | null>(null);

  nameError = computed(() => {
    const name = this.form().name.trim();
    if (!name) return 'Enter a name.';
    if (!NAME_PATTERN.test(name)) return "Use lowercase letters, digits and dashes. Don't start or end with a dash.";
    const editingId = this.editing()?.id;
    return this.ownConnections().some((c) => c.id !== editingId && c.name === name)
      ? 'This owner already has a connection with this name.'
      : null;
  });
  baseUrlError = computed(() => {
    const url = this.form().base_url.trim();
    if (url) return fieldChecks.httpUrl(url);
    return this.form().reports_status ? "Enter the base URL for the Git host that has 'Reports status' turned on." : null;
  });
  accessTokenError = computed(() => {
    const { reports_status, access_token } = this.form();
    if (!reports_status || access_token.trim() || this.editing()?.has_access_token) return null;
    return 'Enter an access token to report status.';
  });
  secretError = computed(() => {
    const editing = this.editing();
    const { receives_events, secret } = this.form();
    return editing && receives_events && !editing.has_secret && !secret.trim()
      ? 'Enter or generate a Webhook secret to receive events.'
      : null;
  });
  allowedIpsError = computed(() => {
    const invalid = splitLines(this.form().allowed_ips).filter((entry) => !isIpNetwork(entry));
    return invalid.length > 0 ? 'Not an IP address or CIDR range: ' + invalid.join(', ') : null;
  });
  capabilityError = computed(() =>
    this.form().receives_events || this.form().reports_status ? null : "Turn on 'Receives events' or 'Reports status', or both.",
  );
  private hasFormError = computed(
    () =>
      !!(this.nameError() || this.baseUrlError() || this.accessTokenError() || this.secretError() || this.allowedIpsError() || this.capabilityError()),
  );

  ngOnInit(): void {
    this.load();
  }

  load(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.api.list(this.gitHostConnectionOwner()).subscribe({
      next: (list) => {
        this.connections.set(list);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  patchForm(change: Partial<ConnectionForm>): void {
    this.form.update((form) => ({ ...form, ...change }));
  }

  canEditRow(connection: GitHostConnectionSummary): boolean {
    return connection.can_manage && connection.git_host_type !== 'github';
  }

  canReadDeliveries(connection: GitHostConnectionSummary): boolean {
    return connection.receives_events && connection.git_host_type !== 'github' && (connection.can_manage || connection.owner.type === 'project');
  }

  toggleDeliveries(connection: GitHostConnectionSummary): void {
    this.deliveriesShownFor.update((shown) => (shown === connection.id ? null : connection.id));
  }

  openCreate(): void {
    this.editing.set(null);
    this.form.set(emptyForm());
    this.openDialog();
  }

  openEdit(connection: GitHostConnectionSummary): void {
    if (this.openingId()) return;
    this.openingId.set(connection.id);
    this.api.get(this.gitHostConnectionOwner(), connection.id).subscribe({
      next: (full) => {
        this.openingId.set(null);
        this.editing.set(full);
        this.form.set({
          name: full.name,
          display_name: full.display_name,
          git_host_type: full.git_host_type as GitHostType,
          base_url: full.base_url ?? '',
          access_token: '',
          secret: '',
          allowed_ips: full.allowed_ips.join('\n'),
          receives_events: full.receives_events,
          reports_status: full.reports_status,
        });
        this.openDialog();
      },
      error: (error: unknown) => {
        this.openingId.set(null);
        this.notify.failure("Couldn't load git host connection", error);
      },
    });
  }

  private openDialog(): void {
    this.dialogError.set(null);
    this.formChanges.mark();
    this.dialogOpen.set(true);
  }

  generateSecret(): void {
    const bytes = new Uint8Array(32);
    crypto.getRandomValues(bytes);
    this.patchForm({ secret: Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('') });
  }

  save(): void {
    if (this.saving() || this.hasFormError()) return;
    const target = this.editing();
    if (target?.managed) return;
    const request = target
      ? this.api.patch(this.gitHostConnectionOwner(), target.id, this.patchBody(target))
      : this.api.create(this.gitHostConnectionOwner(), this.createBody());
    this.saving.set(true);
    this.dialogError.set(null);
    request.subscribe({
      next: (saved) => {
        this.saving.set(false);
        this.dialogOpen.set(false);
        this.editing.set(null);
        this.notify.success('Git host connection saved');
        if (saved.generated_secret) {
          this.createdConnectionWithOneTimeSecret.set({
            webhookUrl: saved.webhook_url ?? '',
            secret: saved.generated_secret,
            gitHostType: saved.git_host_type,
          });
        }
        this.load();
      },
      error: (error: unknown) => {
        this.saving.set(false);
        this.dialogError.set(apiErrorMessage(error, "Couldn't save git host connection"));
      },
    });
  }

  private createBody(): CreateGitHostConnectionRequest {
    const form = this.form();
    const body: CreateGitHostConnectionRequest = {
      name: form.name.trim(),
      git_host_type: form.git_host_type,
      receives_events: form.receives_events,
      reports_status: form.reports_status,
    };
    if (form.display_name.trim()) body.display_name = form.display_name.trim();
    if (form.base_url.trim()) body.base_url = form.base_url.trim();
    if (form.access_token.trim()) body.access_token = form.access_token.trim();
    if (form.receives_events && form.secret.trim()) body.secret = form.secret.trim();
    if (form.receives_events) body.allowed_ips = splitLines(form.allowed_ips);
    return body;
  }

  private patchBody(target: GitHostConnectionResponse): PatchGitHostConnectionRequest {
    const form = this.form();
    const body: PatchGitHostConnectionRequest = {};
    const name = form.name.trim();
    if (name !== target.name) body.name = name;
    const displayName = form.display_name.trim() || name;
    if (displayName !== target.display_name) body.display_name = displayName;
    if (form.git_host_type !== target.git_host_type) body.git_host_type = form.git_host_type;
    if (form.base_url.trim() !== (target.base_url ?? '')) body.base_url = form.base_url.trim();
    if (form.access_token.trim()) body.access_token = form.access_token.trim();
    if (form.secret.trim()) body.secret = form.secret.trim();
    const allowed = splitLines(form.allowed_ips);
    if (allowed.join('\n') !== target.allowed_ips.join('\n')) body.allowed_ips = allowed;
    if (form.receives_events !== target.receives_events) body.receives_events = form.receives_events;
    if (form.reports_status !== target.reports_status) body.reports_status = form.reports_status;
    return body;
  }

  remove(connection: GitHostConnectionSummary): void {
    this.confirm
      .confirm({
        title: 'Delete git host connection',
        target: formatEntityLabel(connection),
        consequence: 'Tasks that use this connection lose it and stop reporting commit status.',
        isIrreversible: true,
        confirmLabel: 'Delete',
        failure: "Couldn't delete git host connection",
        action: () => this.api.delete(this.gitHostConnectionOwner(), connection.id),
      })
      .subscribe(() => {
        this.connections.update((list) => list.filter((c) => c.id !== connection.id));
        this.notify.success('Git host connection deleted');
      });
  }

  requiredWebhookEvents(gitHostType: string): string {
    return webhookEvents(gitHostType);
  }
}
