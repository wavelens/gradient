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
import { CheckboxModule } from 'primeng/checkbox';
import { TriggersService } from '@core/services/triggers.service';
import { IntegrationsService } from '@core/services/integrations.service';
import { OrganizationsService } from '@core/services/organizations.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { WritableDirective, ManagedDisableDirective, AccessService } from '@shared/access';
import { injectProjectAccess } from '@core/resolvers/inject-access';
import {
  ProjectTrigger,
  TriggerType,
  PollingTriggerConfig,
  CreateTriggerBody,
  UpdateTriggerBody,
  IntegrationSummary,
} from '@core/models';

interface Option<T> {
  label: string;
  value: T;
  disabled?: boolean;
}

interface TriggerFormState {
  type: TriggerType;
  active: boolean;
  interval_secs: number;
  branch: string;
  integration_id: string;
  branches: string;
  tags: string;
  releases_only: boolean;
  actions: string;
  cron: string;
}

const DEFAULT_FORM: TriggerFormState = {
  type: 'polling',
  active: true,
  interval_secs: 300,
  branch: '',
  integration_id: '',
  branches: '',
  tags: '',
  releases_only: false,
  actions: '',
  cron: '',
};

@Component({
  selector: 'app-project-triggers',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    DialogModule,
    ButtonModule,
    InputTextModule,
    SelectModule,
    CheckboxModule,
    LoadingSpinnerComponent,
    WritableDirective,
    ManagedDisableDirective,
  ],
  templateUrl: './project-triggers.component.html',
  styleUrl: './project-triggers.component.scss',
})
export class ProjectTriggersComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private triggersService = inject(TriggersService);
  private integrationsService = inject(IntegrationsService);
  private orgsService = inject(OrganizationsService);
  private accessSvc = inject(AccessService);

  access = injectProjectAccess();

  rowDisabled = computed(
    () =>
      this.firingId() !== null ||
      this.deletingId() !== null ||
      this.accessSvc.shouldDisableInput(this.access()),
  );

  loading = signal(true);
  saving = signal(false);
  deletingId = signal<string | null>(null);
  firingId = signal<string | null>(null);
  fireSuccessId = signal<string | null>(null);

  orgName = '';
  orgDisplayName = signal('');
  projectName = '';

  triggers = signal<ProjectTrigger[]>([]);
  editingTrigger = signal<ProjectTrigger | null>(null);
  showCreateDialog = signal(false);
  showEditDialog = signal(false);
  error = signal<string | null>(null);

  inboundIntegrations = signal<IntegrationSummary[]>([]);

  form: TriggerFormState = { ...DEFAULT_FORM };

  typeOptions: Option<TriggerType>[] = [
    { label: 'Polling', value: 'polling' },
    { label: 'Push (reporter)', value: 'reporter_push' },
    { label: 'Pull Request (reporter)', value: 'reporter_pull_request' },
    { label: 'Time (cron)', value: 'time' },
  ];

  integrationOptions = computed<Option<string>[]>(() => {
    const opts: Option<string>[] = [{ label: '— select integration —', value: '' }];
    for (const i of this.inboundIntegrations()) {
      opts.push({ label: `${i.display_name || i.name} (${i.forge_type})`, value: i.id });
    }
    return opts;
  });

  ngOnInit(): void {
    this.orgName = this.route.snapshot.paramMap.get('org') || '';
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.orgsService.getOrganization(this.orgName).subscribe({
      next: (org) => this.orgDisplayName.set(org.display_name),
      error: () => {},
    });
    this.loadTriggers();
    this.loadIntegrations();
  }

  loadTriggers(): void {
    this.loading.set(true);
    this.triggersService.list(this.orgName, this.projectName).subscribe({
      next: (list) => {
        this.triggers.set(list);
        this.loading.set(false);
      },
      error: () => this.loading.set(false),
    });
  }

  private loadIntegrations(): void {
    this.integrationsService.listOrgIntegrationSummaries(this.orgName).subscribe({
      next: (list) => this.inboundIntegrations.set(list.filter((i) => i.kind === 'inbound')),
      error: () => {},
    });
  }

  startCreate(): void {
    this.form = { ...DEFAULT_FORM };
    this.error.set(null);
    this.showCreateDialog.set(true);
  }

  startEdit(trigger: ProjectTrigger): void {
    const cfg = trigger.config as any;
    this.form = {
      type: trigger.type,
      active: trigger.active,
      interval_secs: cfg.interval_secs ?? 300,
      branch: cfg.branch ?? '',
      integration_id: cfg.integration_id ?? '',
      branches: (cfg.branches ?? []).join(', '),
      tags: (cfg.tags ?? []).join(', '),
      releases_only: cfg.releases_only ?? false,
      actions: (cfg.actions ?? []).join(', '),
      cron: cfg.cron ?? '',
    };
    this.error.set(null);
    this.editingTrigger.set(trigger);
    this.showEditDialog.set(true);
  }

  cancelEdit(): void {
    this.showCreateDialog.set(false);
    this.showEditDialog.set(false);
    this.editingTrigger.set(null);
    this.error.set(null);
  }

  private buildConfig(): object {
    switch (this.form.type) {
      case 'polling': {
        const trimmed = this.form.branch?.trim() ?? '';
        return {
          type: 'polling',
          interval_secs: Math.max(10, this.form.interval_secs),
          branch: trimmed === '' ? null : trimmed,
        } as PollingTriggerConfig;
      }
      case 'reporter_push': {
        const cfg: any = { type: 'reporter_push', integration_id: this.form.integration_id };
        const branches = this.splitList(this.form.branches);
        const tags = this.splitList(this.form.tags);
        if (branches.length) cfg.branches = branches;
        if (tags.length) cfg.tags = tags;
        if (this.form.releases_only) cfg.releases_only = true;
        return cfg;
      }
      case 'reporter_pull_request': {
        const cfg: any = { type: 'reporter_pull_request', integration_id: this.form.integration_id };
        const branches = this.splitList(this.form.branches);
        const actions = this.splitList(this.form.actions);
        if (branches.length) cfg.branches = branches;
        if (actions.length) cfg.actions = actions;
        return cfg;
      }
      case 'time':
        return { type: 'time', cron: this.form.cron.trim() };
    }
  }

  saveCreate(): void {
    this.saving.set(true);
    this.error.set(null);
    const body: CreateTriggerBody = {
      config: this.buildConfig() as any,
      active: this.form.active,
    };
    this.triggersService.create(this.orgName, this.projectName, body).subscribe({
      next: () => {
        this.saving.set(false);
        this.showCreateDialog.set(false);
        this.loadTriggers();
      },
      error: (err) => {
        this.error.set(err?.message || 'Failed to create trigger.');
        this.saving.set(false);
      },
    });
  }

  saveEdit(): void {
    const target = this.editingTrigger();
    if (!target) return;
    this.saving.set(true);
    this.error.set(null);
    const body: UpdateTriggerBody = {
      config: this.buildConfig() as any,
      active: this.form.active,
    };
    this.triggersService.update(this.orgName, this.projectName, target.id, body).subscribe({
      next: () => {
        this.saving.set(false);
        this.showEditDialog.set(false);
        this.editingTrigger.set(null);
        this.loadTriggers();
      },
      error: (err) => {
        this.error.set(err?.message || 'Failed to update trigger.');
        this.saving.set(false);
      },
    });
  }

  deleteTrigger(id: string): void {
    if (!confirm('Delete this trigger? This cannot be undone.')) return;
    this.deletingId.set(id);
    this.triggersService.delete(this.orgName, this.projectName, id).subscribe({
      next: () => {
        this.deletingId.set(null);
        this.loadTriggers();
      },
      error: () => this.deletingId.set(null),
    });
  }

  fireNow(id: string): void {
    this.firingId.set(id);
    this.fireSuccessId.set(null);
    this.triggersService.fireNow(this.orgName, this.projectName, id).subscribe({
      next: () => {
        this.firingId.set(null);
        this.fireSuccessId.set(id);
        setTimeout(() => {
          if (this.fireSuccessId() === id) this.fireSuccessId.set(null);
        }, 3000);
      },
      error: () => this.firingId.set(null),
    });
  }

  typeLabel(type: TriggerType): string {
    switch (type) {
      case 'polling': return 'Polling';
      case 'reporter_push': return 'Push';
      case 'reporter_pull_request': return 'Pull Request';
      case 'time': return 'Cron';
    }
  }

  configSummary(trigger: ProjectTrigger): string {
    const cfg = trigger.config as any;
    switch (trigger.type) {
      case 'polling': {
        let summary = `every ${this.formatSeconds(cfg.interval_secs ?? 300)}`;
        if (cfg.branch) summary += `, branch ${cfg.branch}`;
        return summary;
      }
      case 'reporter_push': {
        const parts: string[] = [`from ${this.integrationLabel(trigger)}`];
        if (cfg.branches?.length) parts.push(`branches: ${cfg.branches.join(', ')}`);
        if (cfg.tags?.length) parts.push(`tags: ${cfg.tags.join(', ')}`);
        if (cfg.releases_only) parts.push('releases only');
        return parts.join(' / ');
      }
      case 'reporter_pull_request': {
        const parts: string[] = [`from ${this.integrationLabel(trigger)}`];
        if (cfg.branches?.length) parts.push(`branches: ${cfg.branches.join(', ')}`);
        if (cfg.actions?.length) parts.push(`actions: ${cfg.actions.join(', ')}`);
        return parts.join(' / ');
      }
      case 'time':
        return cfg.cron ?? '';
    }
  }

  private integrationLabel(trigger: ProjectTrigger): string {
    if (trigger.integration) {
      return trigger.integration.display_name || trigger.integration.name;
    }
    return 'deleted integration';
  }

  relativeTime(isoString: string | null): string {
    if (!isoString) return 'never';
    const ms = Date.now() - new Date(isoString + (isoString.endsWith('Z') ? '' : 'Z')).getTime();
    const secs = Math.floor(ms / 1000);
    if (secs < 60) return `${secs}s ago`;
    const mins = Math.floor(secs / 60);
    if (mins < 60) return `${mins}m ago`;
    const hours = Math.floor(mins / 60);
    if (hours < 24) return `${hours}h ago`;
    return `${Math.floor(hours / 24)}d ago`;
  }

  private formatSeconds(secs: number): string {
    if (secs < 60) return `${secs}s`;
    const m = Math.floor(secs / 60);
    if (m < 60) return `${m} min`;
    return `${Math.floor(m / 60)}h ${m % 60}min`;
  }

  private splitList(raw: string): string[] {
    return raw
      .split(',')
      .map((s) => s.trim())
      .filter((s) => s.length > 0);
  }
}
