/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule, UrlTree } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { IntegrationsService } from '@core/services/integrations.service';
import { TasksService } from '@core/services/tasks.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { injectProject } from '@core/resolvers/project-access.resolver';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import {
  accessTo,
  GitHostType,
  IntegrationKind,
  IntegrationResponse,
  CreateIntegrationRequest,
  PatchIntegrationRequest,
} from '@core/models';
import {
  BadgeComponent,
  ButtonComponent,
  CopyFieldComponent,
  DialogComponent,
  EmptyStateComponent,
  FieldRowComponent,
  FormFieldComponent,
  IconComponent,
  InputDirective,
  LoadStateComponent,
  MessageBannerComponent,
  PageLayoutComponent,
  SelectComponent,
  SettingsSectionComponent,
} from '@gradient/ui/ui';
import { LabelHelpComponent } from '@shared/ui';
import { WritableDirective, ManagedDisableDirective, ServerConfigurationManagedBannerComponent, createMissingPermissionLoadStateError, isViewerProjectMember } from '@shared/access';
import { ConfirmService } from '@shared/confirm';
import { IncomingDeliveriesComponent, SELF_HOSTED_GIT_HOSTS, firstRepositoryHost, gitHostLabel, guessGitHost, webhookEvents } from '@shared/git';
import { ServerTimestampToLocalDateTimePipe, slugify } from '@shared/text';
import { links } from '@shared/routing';
import { fieldChecks, isIpNetwork, splitLines } from '@shared/forms';

interface Option<T> {
  label: string;
  value: T;
}

@Component({
  selector: 'app-integrations',
  standalone: true,
  imports: [
    ServerTimestampToLocalDateTimePipe,
    MessageBannerComponent,
    CommonModule,
    RouterModule,
    FormsModule,
    DialogComponent,
    ButtonComponent,
    InputDirective,
    SelectComponent,
    LoadStateComponent,
    LabelHelpComponent,
    WritableDirective,
    ManagedDisableDirective, ServerConfigurationManagedBannerComponent,
    IconComponent,
    PageLayoutComponent,
    FormFieldComponent,
    EmptyStateComponent,
    SettingsSectionComponent,
    BadgeComponent,
    CopyFieldComponent,
    FieldRowComponent,
    IncomingDeliveriesComponent,
  ],
  templateUrl: './integrations.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './integrations.component.scss',
})
export class IntegrationsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private integrationsService = inject(IntegrationsService);
  private resolvedProject = injectProject();
  private tasksService = inject(TasksService);
  private crumbs = inject(BreadcrumbsService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);

  access = computed(() => accessTo(this.resolvedProject(), 'manageIntegrations'));

  loading = signal(true);
  loadError = signal<unknown>(null);
  saving = signal(false);

  projectName = '';
  breadcrumb = computed(() => this.crumbs.projectSettings(this.projectName, { label: 'Integrations' }));
  missingPermission = computed(() =>
    createMissingPermissionLoadStateError('manageIntegrations', 'project', isViewerProjectMember(this.resolvedProject())),
  );
  integrations = signal<IntegrationResponse[]>([]);
  integrationAccess = computed(
    () => new Map(this.integrations().map((i) => [i.id, { ...this.access(), managed: i.managed }])),
  );

  private readonly namePattern = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/;
  submitAttempted = signal(false);

  get nameError(): string | null {
    const name = this.formData.name.trim();
    if (!name) return 'Enter a name.';
    if (!this.namePattern.test(name)) return 'Lowercase letters, digits and dashes only. Cannot start or end with a dash.';
    const editing = this.editingIntegration();
    const kind = editing?.kind ?? this.formData.kind;
    const taken = this.integrations().some((i) => i.id !== editing?.id && i.kind === kind && i.name === name);
    return taken ? 'An integration of this kind with this name is already in this project.' : null;
  }

  get displayNameError(): string | null {
    return this.formData.display_name.trim() ? null : 'Enter a display name.';
  }

  get endpointUrlError(): string | null {
    return fieldChecks.httpUrl(this.formData.endpoint_url);
  }

  get allowedIpsError(): string | null {
    const invalid = splitLines(this.formData.allowed_ips).filter((entry) => !isIpNetwork(entry));
    return invalid.length > 0 ? 'Not an IP address or CIDR range: ' + invalid.join(', ') : null;
  }

  private kindFieldsError(kind: string): string | null {
    return kind === 'inbound' ? this.allowedIpsError : this.endpointUrlError;
  }

  showCreateDialog = signal(false);
  showEditDialog = signal(false);
  showSecretDialog = signal(false);
  createdInbound = signal<{ integration: IntegrationResponse; secret: string } | null>(null);
  private readonly triggerToReturnTo = this.readTriggerToReturnTo();
  private createdForTrigger = signal<UrlTree | null>(null);
  backToTrigger = computed(() => (this.showSecretDialog() ? null : this.createdForTrigger()));
  editingIntegration = signal<IntegrationResponse | null>(null);
  errorMessage = signal<string | null>(null);


  kindOptions: Option<IntegrationKind>[] = [
    { label: 'Inbound (webhook)', value: 'inbound' },
    { label: 'Outbound (status reporter)', value: 'outbound' },
  ];

  formData: {
    name: string;
    display_name: string;
    kind: IntegrationKind;
    git_host_type: GitHostType;
    endpoint_url: string;
    secret: string;
    access_token: string;
    allowed_ips: string;
  } = {
    name: '',
    display_name: '',
    kind: 'inbound',
    git_host_type: 'gitea',
    endpoint_url: '',
    secret: '',
    access_token: '',
    allowed_ips: '',
  };

  githubAppAvailable = computed(() => this.resolvedProject()?.github_app_available === true);
  githubInstallations = computed(() =>
    this.integrations().filter((i) => i.git_host_type === 'github' && i.kind === 'outbound'),
  );
  githubAppInstalled = computed(() => this.githubInstallations().length > 0);
  githubAppInstallUrl = computed(() => this.resolvedProject()?.github_app_install_url ?? null);


  readonly selfHostedGitHostOptions = SELF_HOSTED_GIT_HOSTS;

  repositoryHost = signal<string | null>(null);

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.loadIntegrations();
    this.tasksService.getTasks(this.projectName).subscribe({
      next: (page) => {
        this.repositoryHost.set(firstRepositoryHost(page.items.map((t) => t.repository)));
        this.openCreateDialogForTrigger();
      },
      error: () => this.openCreateDialogForTrigger(),
    });
  }

  private readTriggerToReturnTo(): { task: string; trigger: string } | null {
    const query = this.route.snapshot.queryParamMap;
    const task = query.get('task');
    const trigger = query.get('trigger');
    return task && trigger ? { task, trigger } : null;
  }

  private openCreateDialogForTrigger(): void {
    const access = this.access();
    if (this.triggerToReturnTo && access.canEdit && !access.managed) this.openCreateDialog();
  }

  loadIntegrations(): void {
    if (!this.access().canEdit) {
      this.loading.set(false);
      return;
    }
    this.loading.set(true);
    this.loadError.set(null);
    this.integrationsService.listProjectIntegrations(this.projectName).subscribe({
      next: (list) => {
        this.integrations.set(list);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  openCreateDialog(): void {
    const host = this.repositoryHost();
    this.formData = {
      name: host ? slugify(host) : '',
      display_name: '',
      kind: 'inbound',
      git_host_type: host ? guessGitHost(host) : 'forgejo',
      endpoint_url: host ? `https://${host}` : '',
      secret: '',
      access_token: '',
      allowed_ips: '',
    };
    this.editingIntegration.set(null);
    this.errorMessage.set(null);
    this.submitAttempted.set(false);
    this.showCreateDialog.set(true);
  }

  generateSecret(): void {
    const bytes = new Uint8Array(32);
    crypto.getRandomValues(bytes);
    this.formData.secret = Array.from(bytes)
      .map((b) => b.toString(16).padStart(2, '0'))
      .join('');
  }

  createIntegration(): void {
    if (this.saving()) return;
    this.submitAttempted.set(true);
    if (this.nameError || this.kindFieldsError(this.formData.kind)) return;

    this.saving.set(true);
    this.errorMessage.set(null);
    const body: CreateIntegrationRequest = {
      name: this.formData.name.trim(),
      kind: this.formData.kind,
      git_host_type: this.formData.git_host_type,
    };
    if (this.formData.display_name.trim()) {
      body.display_name = this.formData.display_name.trim();
    }
    if (this.formData.kind === 'inbound') {
      body.allowed_ips = splitLines(this.formData.allowed_ips);
    } else {
      if (this.formData.endpoint_url.trim()) body.endpoint_url = this.formData.endpoint_url.trim();
      if (this.formData.access_token.trim()) body.access_token = this.formData.access_token.trim();
    }
    this.integrationsService.createProjectIntegration(this.projectName, body).subscribe({
      next: (created) => {
        this.saving.set(false);
        this.showCreateDialog.set(false);
        this.notifyCreated(created);
        const returnTo = this.triggerToReturnTo;
        if (returnTo && created.kind === 'inbound') {
          this.createdForTrigger.set(links.editTrigger(this.projectName, returnTo.task, returnTo.trigger, created.id));
        }
        if (created.generated_secret) {
          this.createdInbound.set({ integration: created, secret: created.generated_secret });
          this.showSecretDialog.set(true);
        }
        this.loadIntegrations();
      },
      error: (error: unknown) => {
        this.errorMessage.set(apiErrorMessage(error, 'Failed to create integration.'));
        this.saving.set(false);
      },
    });
  }

  private notifyCreated(created: IntegrationResponse): void {
    const tasks = created.connected_tasks ?? [];
    this.notify.success(tasks.length > 0 ? `Created integration. Connected to ${tasks.join(', ')}.` : 'Integration created');
  }

  openEditDialog(integration: IntegrationResponse): void {
    this.editingIntegration.set(integration);
    this.formData = {
      name: integration.name,
      display_name: integration.display_name,
      kind: integration.kind as IntegrationKind,
      git_host_type: integration.git_host_type as GitHostType,
      endpoint_url: integration.endpoint_url ?? '',
      secret: '',
      access_token: '',
      allowed_ips: (integration.allowed_ips ?? []).join('\n'),
    };
    this.errorMessage.set(null);
    this.submitAttempted.set(false);
    this.showEditDialog.set(true);
  }

  saveEdit(): void {
    const target = this.editingIntegration();
    if (!target || this.saving()) return;
    this.submitAttempted.set(true);
    if (this.nameError || this.displayNameError || this.kindFieldsError(target.kind)) return;
    const body: PatchIntegrationRequest = {};
    if (this.formData.name.trim() !== target.name) {
      body.name = this.formData.name.trim();
    }
    if (this.formData.display_name.trim() !== target.display_name) {
      body.display_name = this.formData.display_name.trim();
    }
    if (this.formData.git_host_type !== target.git_host_type) {
      body.git_host_type = this.formData.git_host_type;
    }
    if (target.kind === 'outbound') {
      if (this.formData.endpoint_url !== (target.endpoint_url ?? '')) {
        body.endpoint_url = this.formData.endpoint_url;
      }
      if (this.formData.access_token.trim()) {
        body.access_token = this.formData.access_token.trim();
      }
    } else {
      if (this.formData.secret.trim()) {
        body.secret = this.formData.secret.trim();
      }
      const allowed = splitLines(this.formData.allowed_ips);
      if (allowed.join('\n') !== (target.allowed_ips ?? []).join('\n')) {
        body.allowed_ips = allowed;
      }
    }
    if (Object.keys(body).length === 0) {
      this.showEditDialog.set(false);
      this.editingIntegration.set(null);
      return;
    }
    this.saving.set(true);
    this.errorMessage.set(null);
    this.integrationsService.patchProjectIntegration(this.projectName, target.id, body).subscribe({
      next: () => {
        this.saving.set(false);
        this.showEditDialog.set(false);
        this.editingIntegration.set(null);
        this.notify.success('Integration updated');
        this.loadIntegrations();
      },
      error: (error: unknown) => {
        this.errorMessage.set(apiErrorMessage(error, 'Failed to update integration.'));
        this.saving.set(false);
      },
    });
  }

  clearAccessToken(): void {
    const target = this.editingIntegration();
    if (!target) return;
    this.confirm
      .confirm({
        title: 'Clear access token',
        target: target.display_name,
        consequence: `The result of this action is removal of the saved access token for ${target.display_name}.`,
        confirmLabel: 'Clear',
        failure: 'Failed to clear token.',
        action: () => this.integrationsService.patchProjectIntegration(this.projectName, target.id, { access_token: '' }),
      })
      .subscribe(() => this.cleared('Access token cleared'));
  }

  private cleared(message: string): void {
    this.loadIntegrations();
    this.showEditDialog.set(false);
    this.editingIntegration.set(null);
    this.notify.success(message);
  }

  deleteIntegration(integration: IntegrationResponse): void {
    this.confirm
      .confirm({
        title: 'Delete integration',
        target: integration.display_name,
        consequence: `The result of this action is deletion of ${integration.display_name}. Events can no longer go in or out.`,
        isIrreversible: true,
        confirmLabel: 'Delete',
        failure: 'Integration deletion failed.',
        action: () => this.integrationsService.deleteProjectIntegration(this.projectName, integration.id),
      })
      .subscribe(() => {
        this.notify.success('Integration deleted');
        this.loadIntegrations();
      });
  }

  requiredWebhookEvents(integration: IntegrationResponse): string {
    return webhookEvents(integration.git_host_type);
  }

  protected readonly gitHostLabel = gitHostLabel;
}
