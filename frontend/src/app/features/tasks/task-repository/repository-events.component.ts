/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, inject, input, output, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { RouterLink } from '@angular/router';
import { links } from '@shared/routing';
import {
  ButtonComponent,
  CopyFieldComponent,
  DialogComponent,
  FormFieldComponent,
  InputDirective,
  SelectComponent,
  SettingsSectionComponent,
} from '@gradient/ui/ui';
import { GitHostType, TaskRepositoryResponse } from '@core/models';
import { TasksService } from '@core/services/tasks.service';
import { IntegrationsService } from '@core/services/integrations.service';
import { NotifyService } from '@core/services/notify.service';
import { ConfigService } from '@core/services/config.service';
import { AuthService } from '@core/services/auth.service';
import { apiErrorMessage } from '@core/services/api.service';
import { IncomingDeliveriesComponent, SELF_HOSTED_GIT_HOSTS, deliverySummary, gitHostSettingsLinks, guessGitHost, repositoryHost, webhookEvents } from '@shared/git';
import { CurrentTimeSignal, relativeTime, slugify } from '@shared/text';
import { fieldChecks } from '@shared/forms';
import { provisioningIntegration } from './provisioning';
import { accessDisabledReason } from '@shared/access';
import { injectTaskAccess } from '@core/resolvers/inject-access';

@Component({
  selector: 'app-repository-events',
  standalone: true,
  imports: [
    FormsModule,
    RouterLink,
    ButtonComponent,
    CopyFieldComponent,
    DialogComponent,
    FormFieldComponent,
    InputDirective,
    SelectComponent,
    SettingsSectionComponent,
    IncomingDeliveriesComponent,
  ],
  templateUrl: './repository-events.component.html',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class RepositoryEventsComponent {
  protected readonly links = links;
  private tasks = inject(TasksService);
  private integrationsService = inject(IntegrationsService);
  private notify = inject(NotifyService);
  private config = inject(ConfigService);
  private auth = inject(AuthService);
  private clock = inject(CurrentTimeSignal);

  project = input.required<string>();
  task = input.required<string>();
  state = input.required<TaskRepositoryResponse>();
  protected access = injectTaskAccess();
  repositoryEventSettingsReadOnlyReason = computed(() => accessDisabledReason(this.access()));
  changed = output<void>();

  host = computed(() => repositoryHost(this.state().repository));
  isGitHub = computed(() => this.host() === 'github.com');
  githubRepositoryConnectionNextStep = computed(() => {
    if (this.config.githubAppEnabled) return 'integrations';
    return this.auth.user()?.superuser ? 'admin' : 'ask';
  });
  selectedInbound = computed(() => this.state().integrations.find((i) => i.kind === 'inbound' && i.serves_repository) ?? null);
  hostLinks = computed(() => gitHostSettingsLinks(this.state().repository, this.selectedInbound()?.git_host_type ?? null));
  events = computed(() => webhookEvents(this.selectedInbound()?.git_host_type ?? 'forgejo'));
  lastEvent = computed(() => {
    const event = this.state().last_event;
    return event ? `${deliverySummary(event)} · ${relativeTime(event.created_at, this.clock.currentTimeMilliseconds())}` : null;
  });
  lastReport = computed(() => {
    const report = this.state().report_status?.last_delivery;
    if (!report) return null;
    const outcome = report.success ? `HTTP ${report.response_status ?? 200}` : (report.error_message ?? `HTTP ${report.response_status}`);
    return `${outcome} · ${relativeTime(report.delivered_at, this.clock.currentTimeMilliseconds())}`;
  });
  provisioner = computed(() => provisioningIntegration(this.state()));
  installingWebhook = signal(false);

  showConnect = signal(false);
  connecting = signal(false);
  connectError = signal<string | null>(null);
  connectForm = { git_host_type: 'forgejo' as GitHostType, endpoint_url: '', access_token: '' };
  readonly gitHostOptions = SELF_HOSTED_GIT_HOSTS;
  createdSecret = signal<{ url: string; secret: string } | null>(null);
  showSecret = signal(false);

  get endpointError(): string | null {
    return this.connectForm.endpoint_url.trim() ? fieldChecks.httpUrl(this.connectForm.endpoint_url) : 'Enter a base URL.';
  }

  openConnect(): void {
    const host = this.host();
    this.connectForm = {
      git_host_type: host ? guessGitHost(host) : 'forgejo',
      endpoint_url: host ? `https://${host}` : '',
      access_token: '',
    };
    this.connectError.set(null);
    this.showConnect.set(true);
  }

  connect(): void {
    if (this.endpointError || this.connecting()) return;
    const host = this.host() ?? 'git-host';
    this.connecting.set(true);
    this.connectError.set(null);
    const token = this.connectForm.access_token.trim();
    this.integrationsService
      .connectGitHost(this.project(), {
        name: slugify(host),
        display_name: host,
        git_host_type: this.connectForm.git_host_type,
        endpoint_url: this.connectForm.endpoint_url.trim(),
        ...(token ? { access_token: token } : {}),
      })
      .subscribe({
        next: (connected) => {
          this.connecting.set(false);
          this.showConnect.set(false);
          const secret = connected.inbound.generated_secret;
          this.createdSecret.set(secret ? { url: connected.inbound.webhook_url ?? '', secret } : null);
          const revealSecret = () => this.showSecret.set(!!secret);
          this.notify.success('Git host connected');
          this.changed.emit();
          if (connected.outbound && token) {
            this.install(connected.inbound.id, connected.outbound.id, revealSecret);
            if (this.state().deploy_key) this.installDeployKey(connected.outbound.id);
          } else {
            revealSecret();
          }
        },
        error: (error: unknown) => {
          this.connecting.set(false);
          this.connectError.set(apiErrorMessage(error, 'Failed to connect to the Git host'));
        },
      });
  }

  installWebhook(): void {
    const inbound = this.selectedInbound()?.id;
    const outbound = this.provisioner()?.id;
    if (inbound && outbound && !this.installingWebhook()) this.install(inbound, outbound);
  }

  private install(inbound: string, outbound: string, onFailure = () => {}): void {
    this.installingWebhook.set(true);
    this.tasks
      .installWebhook(this.project(), this.task(), { inbound_integration: inbound, outbound_integration: outbound })
      .subscribe({
        next: () => {
          this.installingWebhook.set(false);
          this.notify.success('Webhook installed');
        },
        error: (error: unknown) => {
          this.installingWebhook.set(false);
          this.notify.failure("Couldn't install webhook", error);
          onFailure();
        },
      });
  }

  private installDeployKey(outbound: string): void {
    this.tasks.installDeployKey(this.project(), this.task(), { outbound_integration: outbound }).subscribe({
      next: () => this.notify.success('Deploy key installed'),
      error: (error: unknown) => this.notify.failure("Couldn't install deploy key", error),
    });
  }
}
