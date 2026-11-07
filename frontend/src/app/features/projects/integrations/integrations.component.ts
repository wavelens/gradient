/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { IntegrationsService } from '@core/services/integrations.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { ProjectsService } from '@core/services/projects.service';
import { ProjectAccessService } from '@core/services/project-access.service';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import {
  AccessState,
  GitHostType,
  InboundGitHost,
  IntegrationKind,
  ProjectResponse,
  IntegrationResponse,
  CreateIntegrationRequest,
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
  TabSwitchComponent,
} from '@gradient/ui/ui';
import { LabelHelpComponent } from '@shared/ui';
import { WritableDirective, ManagedDisableDirective } from '@shared/access';
import { ConfirmService } from '@shared/confirm';

interface Option<T> {
  label: string;
  value: T;
}

@Component({
  selector: 'app-integrations',
  standalone: true,
  imports: [
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
    ManagedDisableDirective,
    IconComponent,
    PageLayoutComponent,
    FormFieldComponent,
    EmptyStateComponent,
    SettingsSectionComponent,
    BadgeComponent,
    CopyFieldComponent,
    TabSwitchComponent,
    FieldRowComponent,
  ],
  templateUrl: './integrations.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './integrations.component.scss',
})
export class IntegrationsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private integrationsService = inject(IntegrationsService);
  private projectsService = inject(ProjectsService);
  private projectAccess = inject(ProjectAccessService);
  private crumbs = inject(BreadcrumbsService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);

  access = signal<AccessState>({ managed: false, canEdit: false, canTrigger: false });

  loading = signal(true);
  loadError = signal<unknown>(null);
  saving = signal(false);

  projectName = '';
  breadcrumb = computed(() => this.crumbs.projectSettings(this.projectName, { label: 'Integrations' }));
  project = signal<ProjectResponse | null>(null);
  integrations = signal<IntegrationResponse[]>([]);
  integrationAccess = computed(
    () => new Map(this.integrations().map((i) => [i.id, { ...this.access(), managed: i.managed }])),
  );

  private readonly namePattern = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/;

  get nameInvalid(): boolean {
    const n = this.formData.name.trim();
    return n.length > 0 && !this.namePattern.test(n);
  }

  showCreateDialog = signal(false);
  showEditDialog = signal(false);
  editingIntegration = signal<IntegrationResponse | null>(null);
  errorMessage = signal<string | null>(null);

  selectedGitHostByIntegration = signal<Record<string, InboundGitHost>>({});
  copiedUrlId = signal<string | null>(null);

  kindOptions: Option<IntegrationKind>[] = [
    { label: 'Inbound (webhook)', value: 'inbound' },
    { label: 'Outbound (status reporter)', value: 'outbound' },
  ];

  inboundGitHostOptions: Option<InboundGitHost>[] = [
    { label: 'Gitea', value: 'gitea' },
    { label: 'Forgejo', value: 'forgejo' },
    { label: 'GitLab', value: 'gitlab' },
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
    installation_id: string;
  } = {
    name: '',
    display_name: '',
    kind: 'inbound',
    git_host_type: 'gitea',
    endpoint_url: '',
    secret: '',
    access_token: '',
    allowed_ips: '',
    installation_id: '',
  };

  githubAppAvailable = computed(() => this.project()?.github_app_available === true);
  githubInstallations = computed(() =>
    this.integrations().filter((i) => i.git_host_type === 'github' && i.kind === 'outbound'),
  );
  githubAppInstalled = computed(() => this.githubInstallations().length > 0);
  githubAppInstallUrl = computed(() => this.project()?.github_app_install_url ?? null);

  outboundGitHostOptions = computed<Option<GitHostType>[]>(() => [
    { label: 'Gitea', value: 'gitea' },
    { label: 'Forgejo', value: 'forgejo' },
    { label: 'GitLab', value: 'gitlab' },
    { label: 'GitHub', value: 'github' },
  ]);

  allGitHostOptions = computed<Option<GitHostType>[]>(() => [
    { label: 'Gitea', value: 'gitea' },
    { label: 'Forgejo', value: 'forgejo' },
    { label: 'GitLab', value: 'gitlab' },
    { label: 'GitHub', value: 'github' },
  ]);

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.projectAccess.forProject(this.projectName).then((s) => this.access.set(s));
    this.loadProject();
    this.loadIntegrations();
  }

  private loadProject(): void {
    this.projectsService.getProject(this.projectName).subscribe({
      next: (project) => {
        this.project.set(project);
        this.crumbs.rememberProject(this.projectName, project.display_name);
      },
      error: () => {},
    });
  }

  loadIntegrations(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.integrationsService.listProjectIntegrations(this.projectName).subscribe({
      next: (list) => {
        this.integrations.set(list);
        const map: Record<string, InboundGitHost> = {};
        for (const i of list) {
          if (i.kind === 'inbound') {
            map[i.id] =
              i.git_host_type === 'gitea' || i.git_host_type === 'forgejo' || i.git_host_type === 'gitlab'
                ? i.git_host_type
                : 'gitea';
          }
        }
        this.selectedGitHostByIntegration.set(map);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  openCreateDialog(): void {
    this.formData = {
      name: '',
      display_name: '',
      kind: 'inbound',
      git_host_type: 'gitea',
      endpoint_url: '',
      secret: '',
      access_token: '',
      allowed_ips: '',
      installation_id: '',
    };
    this.errorMessage.set(null);
    this.showCreateDialog.set(true);
  }

  private parseAllowedIps(): string[] {
    return this.formData.allowed_ips
      .split('\n')
      .map((s) => s.trim())
      .filter((s) => s.length > 0);
  }

  generateSecret(): void {
    const bytes = new Uint8Array(32);
    crypto.getRandomValues(bytes);
    this.formData.secret = Array.from(bytes)
      .map((b) => b.toString(16).padStart(2, '0'))
      .join('');
  }

  createIntegration(): void {
    if (!this.formData.name.trim() || this.nameInvalid) return;

    if (this.formData.git_host_type === 'github') {
      const installationId = Number(this.formData.installation_id.trim());
      if (!Number.isInteger(installationId) || installationId <= 0) {
        this.errorMessage.set('App installation ID must be a positive integer.');
        return;
      }
      this.saving.set(true);
      this.errorMessage.set(null);
      const body: CreateIntegrationRequest = {
        name: this.formData.name.trim(),
        kind: this.formData.kind,
        git_host_type: 'github',
        installation_id: installationId,
        ...(this.formData.display_name.trim() ? { display_name: this.formData.display_name.trim() } : {}),
      };
      this.integrationsService.createProjectIntegration(this.projectName, body).subscribe({
        next: () => {
          this.saving.set(false);
          this.showCreateDialog.set(false);
          this.notify.success('Integration created');
          this.loadIntegrations();
        },
        error: (error: unknown) => {
          this.errorMessage.set(apiErrorMessage(error, 'Failed to create integration.'));
          this.saving.set(false);
        },
      });
      return;
    }

    this.saving.set(true);
    this.errorMessage.set(null);
    const body: any = {
      name: this.formData.name.trim(),
      kind: this.formData.kind,
      git_host_type: this.formData.git_host_type,
    };
    if (this.formData.display_name.trim()) {
      body.display_name = this.formData.display_name.trim();
    }
    if (this.formData.kind === 'inbound') {
      if (this.formData.secret.trim()) body.secret = this.formData.secret.trim();
      body.allowed_ips = this.parseAllowedIps();
    } else {
      if (this.formData.endpoint_url.trim()) body.endpoint_url = this.formData.endpoint_url.trim();
      if (this.formData.access_token.trim()) body.access_token = this.formData.access_token.trim();
    }
    this.integrationsService.createProjectIntegration(this.projectName, body).subscribe({
      next: () => {
        this.saving.set(false);
        this.showCreateDialog.set(false);
        this.notify.success('Integration created');
        this.loadIntegrations();
      },
      error: (error: unknown) => {
        this.errorMessage.set(apiErrorMessage(error, 'Failed to create integration.'));
        this.saving.set(false);
      },
    });
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
      installation_id: '',
    };
    this.errorMessage.set(null);
    this.showEditDialog.set(true);
  }

  saveEdit(): void {
    const target = this.editingIntegration();
    if (!target) return;
    this.saving.set(true);
    this.errorMessage.set(null);
    const body: any = {};
    if (this.formData.name.trim() && this.formData.name !== target.name) {
      body.name = this.formData.name.trim();
    }
    if (this.formData.display_name.trim() && this.formData.display_name !== target.display_name) {
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
      const allowed = this.parseAllowedIps();
      const current = (target.allowed_ips ?? []).join('\n');
      if (this.formData.allowed_ips !== current) {
        body.allowed_ips = allowed;
      }
    }
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

  clearSecret(): void {
    const target = this.editingIntegration();
    if (!target) return;
    this.confirm
      .confirm({
        title: 'Clear webhook secret',
        target: target.display_name,
        consequence: `The result of this action is removal of the saved webhook secret for ${target.display_name}.`,
        confirmLabel: 'Clear',
        failure: 'Failed to clear secret.',
        action: () => this.integrationsService.patchProjectIntegration(this.projectName, target.id, { secret: '' }),
      })
      .subscribe(() => this.cleared('Webhook secret cleared'));
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
        consequence: `The result of this action is deletion of ${integration.display_name}. Events can no longer go in or out. This action is permanent.`,
        confirmLabel: 'Delete',
        failure: 'Integration deletion failed.',
        action: () => this.integrationsService.deleteProjectIntegration(this.projectName, integration.id),
      })
      .subscribe(() => {
        this.notify.success('Integration deleted');
        this.loadIntegrations();
      });
  }

  inboundGitHost(id: string): InboundGitHost {
    return this.selectedGitHostByIntegration()[id] ?? 'gitea';
  }

  setInboundGitHost(id: string, gitHost: InboundGitHost): void {
    this.selectedGitHostByIntegration.update((m) => ({ ...m, [id]: gitHost }));
  }

  inboundUrl(integration: IntegrationResponse): string {
    const gitHost = this.inboundGitHost(integration.id);
    return `${window.location.origin}/api/v1/hooks/${gitHost}/${this.projectName}/${integration.name}`;
  }

  requiredWebhookEvents(id: string): string {
    return this.inboundGitHost(id) === 'gitlab'
      ? 'Push, Tag push, Merge request, Comments (note), and Releases events'
      : 'Push, Pull Request, Issue Comment, Pull Request Comment, Pull Request Review, and Release';
  }

  copyInboundUrl(integration: IntegrationResponse): void {
    const url = this.inboundUrl(integration);
    navigator.clipboard.writeText(url).then(() => {
      this.copiedUrlId.set(integration.id);
      setTimeout(() => {
        if (this.copiedUrlId() === integration.id) this.copiedUrlId.set(null);
      }, 2000);
    });
  }

  gitHostLabel(gitHost: string): string {
    switch (gitHost) {
      case 'gitea': return 'Gitea';
      case 'forgejo': return 'Forgejo';
      case 'gitlab': return 'GitLab';
      case 'github': return 'GitHub';
      default: return gitHost;
    }
  }

  formatDate(s: string): string {
    try {
      return new Date(s).toLocaleString();
    } catch {
      return s;
    }
  }
}
