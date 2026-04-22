/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { DialogModule } from 'primeng/dialog';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';
import { SelectModule } from 'primeng/select';
import { ToggleSwitchModule } from 'primeng/toggleswitch';
import { IntegrationsService } from '@core/services/integrations.service';
import { OrganizationsService } from '@core/services/organizations.service';
import {
  ForgeType,
  InboundForge,
  Integration,
  IntegrationKind,
  Organization,
} from '@core/models';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';

interface Option<T> {
  label: string;
  value: T;
}

@Component({
  selector: 'app-integrations',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    DialogModule,
    ButtonModule,
    InputTextModule,
    SelectModule,
    ToggleSwitchModule,
    LoadingSpinnerComponent,
  ],
  templateUrl: './integrations.component.html',
  styleUrl: './integrations.component.scss',
})
export class IntegrationsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private integrationsService = inject(IntegrationsService);
  private orgsService = inject(OrganizationsService);

  loading = signal(true);
  saving = signal(false);
  deletingId = signal<string | null>(null);
  togglingGithub = signal(false);

  orgName = '';
  orgDisplayName = signal('');
  organization = signal<Organization | null>(null);
  integrations = signal<Integration[]>([]);

  showCreateDialog = signal(false);
  showEditDialog = signal(false);
  editingIntegration = signal<Integration | null>(null);
  errorMessage = signal<string | null>(null);

  selectedForgeByIntegration = signal<Record<string, InboundForge>>({});
  copiedUrlId = signal<string | null>(null);

  kindOptions: Option<IntegrationKind>[] = [
    { label: 'Inbound (webhook)', value: 'inbound' },
    { label: 'Outbound (status reporter)', value: 'outbound' },
  ];

  inboundForgeOptions: Option<InboundForge>[] = [
    { label: 'Gitea', value: 'gitea' },
    { label: 'Forgejo', value: 'forgejo' },
    { label: 'GitLab', value: 'gitlab' },
  ];

  formData: {
    name: string;
    display_name: string;
    kind: IntegrationKind;
    forge_type: ForgeType;
    endpoint_url: string;
    secret: string;
    access_token: string;
  } = {
    name: '',
    display_name: '',
    kind: 'inbound',
    forge_type: 'gitea',
    endpoint_url: '',
    secret: '',
    access_token: '',
  };

  githubAppAvailable = computed(() => this.organization()?.github_app_available === true);
  githubAppEnabled = signal(false);

  outboundForgeOptions = computed<Option<ForgeType>[]>(() => {
    const base: Option<ForgeType>[] = [
      { label: 'Gitea', value: 'gitea' },
      { label: 'Forgejo', value: 'forgejo' },
      { label: 'GitLab', value: 'gitlab' },
    ];
    if (this.githubAppAvailable()) {
      base.push({ label: 'GitHub', value: 'github' });
    }
    return base;
  });

  allForgeOptions = computed<Option<ForgeType>[]>(() => {
    const base: Option<ForgeType>[] = [
      { label: 'Gitea', value: 'gitea' },
      { label: 'Forgejo', value: 'forgejo' },
      { label: 'GitLab', value: 'gitlab' },
    ];
    if (this.githubAppAvailable()) {
      base.push({ label: 'GitHub', value: 'github' });
    }
    return base;
  });

  ngOnInit(): void {
    this.orgName = this.route.snapshot.paramMap.get('org') || '';
    this.loadOrganization();
    this.loadIntegrations();
  }

  private loadOrganization(): void {
    this.orgsService.getOrganization(this.orgName).subscribe({
      next: (org) => {
        this.organization.set(org);
        this.orgDisplayName.set(org.display_name);
        this.githubAppEnabled.set(org.github_app_enabled ?? false);
      },
      error: () => {},
    });
  }

  loadIntegrations(): void {
    this.loading.set(true);
    this.integrationsService.listOrgIntegrations(this.orgName).subscribe({
      next: (list) => {
        this.integrations.set(list);
        const map: Record<string, InboundForge> = {};
        for (const i of list) {
          if (i.kind === 'inbound') {
            map[i.id] =
              i.forge_type === 'gitea' || i.forge_type === 'forgejo' || i.forge_type === 'gitlab'
                ? i.forge_type
                : 'gitea';
          }
        }
        this.selectedForgeByIntegration.set(map);
        this.loading.set(false);
      },
      error: () => this.loading.set(false),
    });
  }

  openCreateDialog(): void {
    this.formData = {
      name: '',
      display_name: '',
      kind: 'inbound',
      forge_type: 'gitea',
      endpoint_url: '',
      secret: '',
      access_token: '',
    };
    this.errorMessage.set(null);
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
    if (!this.formData.name.trim()) return;
    this.saving.set(true);
    this.errorMessage.set(null);
    const body: any = {
      name: this.formData.name.trim(),
      kind: this.formData.kind,
      forge_type: this.formData.forge_type,
    };
    if (this.formData.display_name.trim()) {
      body.display_name = this.formData.display_name.trim();
    }
    if (this.formData.kind === 'inbound') {
      if (this.formData.secret.trim()) body.secret = this.formData.secret.trim();
    } else {
      if (this.formData.endpoint_url.trim()) body.endpoint_url = this.formData.endpoint_url.trim();
      if (this.formData.access_token.trim()) body.access_token = this.formData.access_token.trim();
    }
    this.integrationsService.createOrgIntegration(this.orgName, body).subscribe({
      next: () => {
        this.saving.set(false);
        this.showCreateDialog.set(false);
        this.loadIntegrations();
      },
      error: (err) => {
        this.errorMessage.set(err?.message || 'Failed to create integration.');
        this.saving.set(false);
      },
    });
  }

  openEditDialog(integration: Integration): void {
    this.editingIntegration.set(integration);
    this.formData = {
      name: integration.name,
      display_name: integration.display_name,
      kind: integration.kind,
      forge_type: integration.forge_type,
      endpoint_url: integration.endpoint_url ?? '',
      secret: '',
      access_token: '',
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
    if (this.formData.forge_type !== target.forge_type) {
      body.forge_type = this.formData.forge_type;
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
    }
    this.integrationsService.patchOrgIntegration(this.orgName, target.id, body).subscribe({
      next: () => {
        this.saving.set(false);
        this.showEditDialog.set(false);
        this.editingIntegration.set(null);
        this.loadIntegrations();
      },
      error: (err) => {
        this.errorMessage.set(err?.message || 'Failed to update integration.');
        this.saving.set(false);
      },
    });
  }

  clearSecret(): void {
    const target = this.editingIntegration();
    if (!target) return;
    this.saving.set(true);
    this.errorMessage.set(null);
    this.integrationsService
      .patchOrgIntegration(this.orgName, target.id, { secret: '' })
      .subscribe({
        next: () => {
          this.saving.set(false);
          this.loadIntegrations();
          this.showEditDialog.set(false);
          this.editingIntegration.set(null);
        },
        error: (err) => {
          this.errorMessage.set(err?.message || 'Failed to clear secret.');
          this.saving.set(false);
        },
      });
  }

  clearAccessToken(): void {
    const target = this.editingIntegration();
    if (!target) return;
    this.saving.set(true);
    this.errorMessage.set(null);
    this.integrationsService
      .patchOrgIntegration(this.orgName, target.id, { access_token: '' })
      .subscribe({
        next: () => {
          this.saving.set(false);
          this.loadIntegrations();
          this.showEditDialog.set(false);
          this.editingIntegration.set(null);
        },
        error: (err) => {
          this.errorMessage.set(err?.message || 'Failed to clear token.');
          this.saving.set(false);
        },
      });
  }

  deleteIntegration(integration: Integration): void {
    this.deletingId.set(integration.id);
    this.integrationsService.deleteOrgIntegration(this.orgName, integration.id).subscribe({
      next: () => {
        this.deletingId.set(null);
        this.loadIntegrations();
      },
      error: () => this.deletingId.set(null),
    });
  }

  inboundForge(id: string): InboundForge {
    return this.selectedForgeByIntegration()[id] ?? 'gitea';
  }

  setInboundForge(id: string, forge: InboundForge): void {
    this.selectedForgeByIntegration.update((m) => ({ ...m, [id]: forge }));
  }

  inboundUrl(integration: Integration): string {
    const forge = this.inboundForge(integration.id);
    return `${window.location.origin}/api/v1/hooks/${forge}/${this.orgName}/${integration.name}`;
  }

  copyInboundUrl(integration: Integration): void {
    const url = this.inboundUrl(integration);
    navigator.clipboard.writeText(url).then(() => {
      this.copiedUrlId.set(integration.id);
      setTimeout(() => {
        if (this.copiedUrlId() === integration.id) this.copiedUrlId.set(null);
      }, 2000);
    });
  }

  toggleGithubApp(): void {
    const next = !this.githubAppEnabled();
    this.togglingGithub.set(true);
    this.integrationsService.setGithubAppEnabled(this.orgName, next).subscribe({
      next: () => {
        this.githubAppEnabled.set(next);
        this.togglingGithub.set(false);
      },
      error: () => this.togglingGithub.set(false),
    });
  }

  onGithubAppToggle(enabled: boolean): void {
    this.togglingGithub.set(true);
    this.integrationsService.setGithubAppEnabled(this.orgName, enabled).subscribe({
      next: () => {
        this.githubAppEnabled.set(enabled);
        this.togglingGithub.set(false);
      },
      error: () => {
        this.githubAppEnabled.set(!enabled);
        this.togglingGithub.set(false);
      },
    });
  }

  forgeLabel(forge: ForgeType): string {
    switch (forge) {
      case 'gitea': return 'Gitea';
      case 'forgejo': return 'Forgejo';
      case 'gitlab': return 'GitLab';
      case 'github': return 'GitHub';
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
