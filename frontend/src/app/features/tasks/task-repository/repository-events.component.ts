/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, effect, inject, input, output, signal, untracked } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { RouterLink } from '@angular/router';
import { links } from '@shared/routing';
import {
  ButtonComponent,
  CheckboxComponent,
  CopyFieldComponent,
  DialogComponent,
  FormFieldComponent,
  InputDirective,
  SelectComponent,
  SettingsSectionComponent,
} from '@gradient/ui/ui';
import { GitHostType, RepositoryIntegration, TaskRepositoryResponse, UpdateTaskRepositoryRequest } from '@core/models';
import { TasksService } from '@core/services/tasks.service';
import { IntegrationsService } from '@core/services/integrations.service';
import { NotifyService } from '@core/services/notify.service';
import { ConfigService } from '@core/services/config.service';
import { AuthService } from '@core/services/auth.service';
import { apiErrorMessage } from '@core/services/api.service';
import { IncomingDeliveriesComponent, SELF_HOSTED_GIT_HOSTS, deliverySummary, gitHostSettingsLinks, guessGitHost, repositoryHost, webhookEvents } from '@shared/git';
import { CurrentTimeSignal, relativeTime, slugify } from '@shared/text';
import { fieldChecks, splitList, unsavedChanges } from '@shared/forms';
import { provisioningIntegration } from './provisioning';
import { ManagedDisableDirective, accessDisabledReason } from '@shared/access';
import { injectTaskAccess } from '@core/resolvers/inject-access';

interface EventsForm {
  inbound: string | null;
  push: boolean;
  pushBranches: string;
  pushTags: string;
  pullRequest: boolean;
  pullRequestBranches: string;
  requireApproval: boolean;
  outbound: string | null;
  reportStatus: boolean;
}

interface Option<T> {
  label: string;
  value: T;
}

function formOf(state: TaskRepositoryResponse): EventsForm {
  const push = state.build_on_push;
  const pr = state.build_on_pull_request;
  const servingInbound = state.integrations.find((i) => i.kind === 'inbound' && i.serves_repository);
  const servingOutbound = state.integrations.find((i) => i.kind === 'outbound' && i.serves_repository);
  return {
    inbound: push?.integration_id ?? pr?.integration_id ?? servingInbound?.id ?? null,
    push: push?.active ?? false,
    pushBranches: (push?.branches ?? []).join(', '),
    pushTags: (push?.tags ?? []).join(', '),
    pullRequest: pr?.active ?? false,
    pullRequestBranches: (pr?.branches ?? []).join(', '),
    requireApproval: pr?.require_approval ?? true,
    outbound: state.report_status?.integration_id ?? servingOutbound?.id ?? null,
    reportStatus: state.report_status?.active ?? false,
  };
}

function integrationLabel(i: RepositoryIntegration): string {
  return i.serves_repository ? i.display_name : `${i.display_name} (different host)`;
}

@Component({
  selector: 'app-repository-events',
  standalone: true,
  imports: [
    FormsModule,
    RouterLink,
    ButtonComponent,
    CheckboxComponent,
    CopyFieldComponent,
    DialogComponent,
    FormFieldComponent,
    InputDirective,
    SelectComponent,
    SettingsSectionComponent,
    IncomingDeliveriesComponent,
    ManagedDisableDirective,
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
  saved = output<TaskRepositoryResponse>();

  form = signal<EventsForm>({
    inbound: null,
    push: false,
    pushBranches: '',
    pushTags: '',
    pullRequest: false,
    pullRequestBranches: '',
    requireApproval: true,
    outbound: null,
    reportStatus: false,
  });
  readonly unsaved = unsavedChanges(() => this.form(), 'Repository');
  saving = signal(false);
  saveError = signal<string | null>(null);

  host = computed(() => repositoryHost(this.state().repository));
  isGitHub = computed(() => this.host() === 'github.com');
  githubRepositoryConnectionNextStep = computed(() => {
    if (this.config.githubAppEnabled) return 'integrations';
    return this.auth.user()?.superuser ? 'admin' : 'ask';
  });
  inboundOptions = computed(() => this.optionsOf('inbound'));
  outboundOptions = computed(() => this.optionsOf('outbound'));
  selectedInbound = computed(() => this.state().integrations.find((i) => i.id === this.form().inbound) ?? null);
  hasServingInbound = computed(() => this.state().integrations.some((i) => i.kind === 'inbound' && i.serves_repository));
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
  formError = computed(() => {
    const f = this.form();
    if ((f.push || f.pullRequest) && !f.inbound) return 'Select a connection for events first.';
    if (f.reportStatus && !f.outbound) return 'Select a connection for status reports first.';
    return null;
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

  constructor() {
    effect(() => {
      const next = formOf(this.state());
      this.form.set(next);
      untracked(() => this.unsaved.mark());
    });
  }

  dirty(): boolean {
    return this.unsaved.changed();
  }

  patch(change: Partial<EventsForm>): void {
    this.form.update((f) => {
      const next = { ...f, ...change };
      if (change.inbound && !f.inbound && !f.push && !f.pullRequest) {
        next.push = true;
        next.pullRequest = true;
      }
      if (change.outbound && !f.outbound && !f.reportStatus) next.reportStatus = true;
      return next;
    });
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
          const base = this.form();
          this.persist({
            ...base,
            inbound: connected.inbound.id,
            push: true,
            pullRequest: true,
            outbound: connected.outbound?.id ?? base.outbound,
            reportStatus: !!connected.outbound || base.reportStatus,
          }, 'Git host connected');
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
    const inbound = this.form().inbound;
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

  save(): void {
    if (this.formError() || this.saving()) return;
    this.persist(this.form());
  }

  private persist(form: EventsForm, successMessage = 'Repository settings saved'): void {
    this.saving.set(true);
    this.saveError.set(null);
    const body: UpdateTaskRepositoryRequest = {
      inbound_integration: form.inbound,
      build_on_push: form.push,
      push_branches: splitList(form.pushBranches),
      push_tags: splitList(form.pushTags),
      build_on_pull_request: form.pullRequest,
      pull_request_branches: splitList(form.pullRequestBranches),
      require_approval: form.requireApproval,
      outbound_integration: form.outbound,
      report_status: form.reportStatus,
    };
    this.tasks.updateRepository(this.project(), this.task(), body).subscribe({
      next: (state) => {
        this.saving.set(false);
        this.notify.success(successMessage);
        this.saved.emit(state);
      },
      error: (error: unknown) => {
        this.saving.set(false);
        this.saveError.set(apiErrorMessage(error, 'Failed to save repository settings'));
        this.notify.failure('Failed to save repository settings', error);
      },
    });
  }

  private optionsOf(kind: 'inbound' | 'outbound'): Option<string | null>[] {
    const integrations = this.state()
      .integrations.filter((i) => i.kind === kind)
      .sort((a, b) => Number(b.serves_repository) - Number(a.serves_repository));
    return [
      { label: kind === 'inbound' ? 'None (scheduled check only)' : 'None', value: null },
      ...integrations.map((i) => ({ label: integrationLabel(i), value: i.id })),
    ];
  }
}
