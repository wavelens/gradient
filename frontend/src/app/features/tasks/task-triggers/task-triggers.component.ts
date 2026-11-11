/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { TriggersService } from '@core/services/triggers.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import { ConfirmService } from '@shared/confirm';
import {
  BadgeComponent,
  ButtonComponent,
  CheckboxComponent,
  DialogComponent,
  FormFieldComponent,
  IconComponent,
  InputDirective,
  LoadStateComponent,
  MessageBannerComponent,
  PageLayoutComponent,
  SelectComponent,
  SettingsSectionComponent,
  TooltipDirective,
} from '@gradient/ui/ui';
import { LabelHelpComponent } from '@shared/ui';
import { WritableDirective, ManagedDisableDirective, ServerConfigurationManagedBannerComponent, AccessService, isViewerProjectMember, requestIfProjectMember } from '@shared/access';
import { injectTaskAccess, injectTaskAccessData, injectTaskAccessTo } from '@core/resolvers/inject-access';
import {
  TriggerType,
  TriggerOut,
  TriggerConfig,
  CreateTriggerRequest,
  UpdateTriggerRequest,
} from '@core/models';
import { CurrentTimeSignal, RelativeTimePipe } from '@shared/text';
import { links } from '@shared/routing';
import { splitList } from '@shared/forms';
import { getCiTriggerTypeLabel, getNextPollTime, nextPollText } from '@shared/evaluation';
import { cronExpressionToPlainEnglish } from './cron-expression-description';

interface Option<T> {
  label: string;
  value: T;
  disabled?: boolean;
}

interface TriggerFormState {
  type: TriggerType;
  active: boolean;
  interval_secs: number | null;
  branch: string;
  branches: string;
  tags: string;
  releases_only: boolean;
  include_flake_inputs: boolean;
  actions: string;
  require_approval: boolean;
  cron: string;
}

const DEFAULT_FORM: TriggerFormState = {
  type: 'polling',
  active: true,
  interval_secs: 300,
  branch: '',
  branches: '',
  tags: '',
  releases_only: false,
  include_flake_inputs: false,
  actions: '',
  require_approval: true,
  cron: '',
};

@Component({
  selector: 'app-task-triggers',
  standalone: true,
  imports: [
    ServerConfigurationManagedBannerComponent,
    RelativeTimePipe,
    LabelHelpComponent,
    CommonModule,
    RouterModule,
    FormsModule,
    DialogComponent,
    ButtonComponent,
    InputDirective,
    SelectComponent,
    CheckboxComponent,
    LoadStateComponent,
    WritableDirective,
    ManagedDisableDirective,
    IconComponent,
    PageLayoutComponent,
    FormFieldComponent,
    BadgeComponent,
    TooltipDirective,
    MessageBannerComponent,
    SettingsSectionComponent,
  ],
  templateUrl: './task-triggers.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './task-triggers.component.scss',
})
export class TaskTriggersComponent implements OnInit {
  protected readonly links = links;
  private route = inject(ActivatedRoute);
  private triggersService = inject(TriggersService);
  private crumbs = inject(BreadcrumbsService);
  private accessSvc = inject(AccessService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);
  private clock = inject(CurrentTimeSignal);

  private taskAccess = injectTaskAccess();
  private taskAccessData = injectTaskAccessData();
  viewerIsProjectMember = computed(() => isViewerProjectMember(this.taskAccessData()?.task));
  access = injectTaskAccessTo('manageTriggers');

  rowBusy = computed(() => this.firingId() !== null || this.makingDefaultId() !== null);

  triggerAccess = computed(() => this.accessSvc.triggerAccess(this.taskAccess()));

  triggerRowDisabled = computed(
    () =>
      this.firingId() !== null ||
      this.accessSvc.shouldDisableInput(this.triggerAccess()),
  );

  loading = signal(true);
  loadError = signal<unknown>(null);
  saving = signal(false);
  makingDefaultId = signal<string | null>(null);
  firingId = signal<string | null>(null);
  fireSuccessId = signal<string | null>(null);

  projectName = '';
  taskName = '';
  breadcrumb = computed(() => this.crumbs.taskSettings(this.projectName, this.taskName, { label: 'Triggers' }));

  triggers = signal<TriggerOut[]>([]);
  editingTrigger = signal<TriggerOut | null>(null);
  showCreateDialog = signal(false);
  showEditDialog = signal(false);
  error = signal<string | null>(null);

  private suggestions = signal<TriggerConfig[]>([]);
  missingTriggerConfigSuggestions = computed(() =>
    this.suggestions().filter((suggestion) =>
      !this.triggers().some((t) => t.type === suggestion.type),
    ),
  );
  fallbackPollingTrigger = computed(() => {
    const active = this.triggers().filter((t) => t.active);
    return active.some((t) => t.type === 'reporter_push') ? (active.find((t) => t.type === 'polling') ?? null) : null;
  });

  form: TriggerFormState = { ...DEFAULT_FORM };
  submitAttempted = signal(false);
  private loadedForm = '';

  readonly typeLabel = getCiTriggerTypeLabel;
  typeOptions: Option<TriggerType>[] = (['polling', 'reporter_push', 'reporter_pull_request', 'time'] as const)
    .map((value) => ({ label: getCiTriggerTypeLabel(value), value }));

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.taskName = this.route.snapshot.paramMap.get('task') || '';
    this.loadTriggers();
    this.loadSuggestions();
  }

  loadTriggers(): void {
    this.loading.set(true);
    this.loadError.set(null);
    requestIfProjectMember(this.taskAccessData()?.task, () => this.triggersService.list(this.projectName, this.taskName)).subscribe({
      next: (list) => {
        this.triggers.set([...list].sort((a, b) => a.created_at.localeCompare(b.created_at) || a.id.localeCompare(b.id)));
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  private loadSuggestions(): void {
    const repository = this.taskAccessData()?.task.repository;
    if (!this.viewerIsProjectMember() || !repository) return;
    this.triggersService.getSuggestedPushAndPullRequestTriggerConfigs(this.projectName, repository).subscribe({
      next: (list) => this.suggestions.set(list),
      error: () => {},
    });
  }

  get intervalError(): string | null {
    const interval = this.form.interval_secs;
    return interval !== null && /^\d+$/.test(String(interval)) && interval >= 10
      ? null
      : 'Enter an integer of at least 10 seconds.';
  }

  get cronError(): string | null {
    const cron = this.form.cron.trim();
    if (!cron) return 'Enter a cron expression.';
    const fields = cron.split(/\s+/).length;
    return fields >= 5 && fields <= 7
      ? null
      : 'Enter 5, 6, or 7 fields, each separated by a space. The 5 fields are minute, hour, day of month, month, and day of week. Use a 6th field at the start to set seconds. Use a 7th field at the end to set year.';
  }

  get cronScheduleDescription(): string | null {
    return cronExpressionToPlainEnglish(this.form.cron);
  }

  private get formError(): string | null {
    switch (this.form.type) {
      case 'polling':
        return this.intervalError;
      case 'reporter_push':
      case 'reporter_pull_request':
        return null;
      case 'time':
        return this.cronError;
    }
  }

  submitForm(): void {
    if (this.editingTrigger()) this.saveEdit();
    else this.saveCreate();
  }

  startCreate(): void {
    this.form = { ...DEFAULT_FORM };
    this.error.set(null);
    this.submitAttempted.set(false);
    this.editingTrigger.set(null);
    this.showCreateDialog.set(true);
  }

  startEdit(trigger: TriggerOut): void {
    const cfg = trigger.config as any;
    this.form = {
      type: trigger.type,
      active: trigger.active,
      interval_secs: cfg.interval_secs ?? 300,
      branch: cfg.branch ?? '',
      branches: (cfg.branches ?? []).join(', '),
      tags: (cfg.tags ?? []).join(', '),
      releases_only: cfg.releases_only ?? false,
      include_flake_inputs: cfg.include_flake_inputs ?? false,
      actions: (cfg.actions ?? []).join(', '),
      require_approval: cfg.require_approval ?? true,
      cron: cfg.cron ?? '',
    };
    this.loadedForm = JSON.stringify(this.form);
    this.error.set(null);
    this.submitAttempted.set(false);
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
    const branch = this.form.branch?.trim() || null;
    switch (this.form.type) {
      case 'polling':
        return { type: 'polling', interval_secs: Number(this.form.interval_secs), branch } as TriggerConfig;
      case 'reporter_push': {
        const cfg: any = { type: 'reporter_push' };
        const branches = splitList(this.form.branches);
        const tags = splitList(this.form.tags);
        if (branches.length) cfg.branches = branches;
        if (tags.length) cfg.tags = tags;
        if (this.form.releases_only) cfg.releases_only = true;
        if (this.form.include_flake_inputs) cfg.include_flake_inputs = true;
        return cfg;
      }
      case 'reporter_pull_request': {
        const cfg: any = { type: 'reporter_pull_request' };
        const branches = splitList(this.form.branches);
        const actions = splitList(this.form.actions);
        if (branches.length) cfg.branches = branches;
        cfg.actions = actions;
        cfg.require_approval = this.form.require_approval !== false;
        return cfg;
      }
      case 'time':
        return { type: 'time', cron: this.form.cron.trim(), branch } as TriggerConfig;
    }
  }

  saveCreate(): void {
    if (this.saving()) return;
    this.submitAttempted.set(true);
    if (this.formError) return;
    this.saving.set(true);
    this.error.set(null);
    const body: CreateTriggerRequest = {
      config: this.buildConfig() as any,
      active: this.form.active,
    };
    this.triggersService.create(this.projectName, this.taskName, body).subscribe({
      next: () => {
        this.saving.set(false);
        this.showCreateDialog.set(false);
        this.notify.success('Trigger created');
        this.loadTriggers();
      },
      error: (err) => {
        this.error.set(apiErrorMessage(err, 'Failed to create trigger.'));
        this.saving.set(false);
      },
    });
  }

  saveEdit(): void {
    const target = this.editingTrigger();
    if (!target || this.saving()) return;
    this.submitAttempted.set(true);
    if (this.formError) return;
    if (JSON.stringify(this.form) === this.loadedForm) {
      this.cancelEdit();
      return;
    }
    this.saving.set(true);
    this.error.set(null);
    const body: UpdateTriggerRequest = {
      config: this.buildConfig() as any,
      active: this.form.active,
    };
    this.triggersService.update(this.projectName, this.taskName, target.id, body).subscribe({
      next: () => {
        this.saving.set(false);
        this.showEditDialog.set(false);
        this.editingTrigger.set(null);
        this.notify.success('Trigger saved');
        this.loadTriggers();
      },
      error: (err) => {
        this.error.set(apiErrorMessage(err, 'Failed to update trigger.'));
        this.saving.set(false);
      },
    });
  }

  deleteTrigger(trigger: TriggerOut): void {
    this.confirm.confirm({
      title: 'Delete Trigger',
      target: `${this.typeLabel(trigger.type)}: ${this.configSummary(trigger)}`,
      consequence: "Evaluations no longer start from this trigger. You can't undo this.",
      confirmLabel: 'Delete',
      failure: 'Failed to delete the trigger.',
      action: () => this.triggersService.delete(this.projectName, this.taskName, trigger.id),
    }).subscribe(() => {
      this.notify.success('Trigger deleted');
      this.loadTriggers();
    });
  }

  deactivateTrigger(trigger: TriggerOut): void {
    this.triggersService.update(this.projectName, this.taskName, trigger.id, { active: false }).subscribe({
      next: () => {
        this.notify.success('Trigger saved');
        this.loadTriggers();
      },
      error: (error: unknown) => this.notify.failure("Couldn't save trigger", error),
    });
  }

  createTriggerFromSuggestedConfig(config: TriggerConfig): void {
    this.triggersService.create(this.projectName, this.taskName, { config, active: true }).subscribe({
      next: () => {
        this.notify.success('Trigger created');
        this.loadTriggers();
      },
      error: (error: unknown) => this.notify.failure("Couldn't create trigger", error),
    });
  }

  nextPoll(trigger: TriggerOut): string | null {
    return nextPollText(getNextPollTime(trigger), this.clock.currentTimeMilliseconds());
  }

  makeDefault(id: string): void {
    this.makingDefaultId.set(id);
    this.triggersService.setDefault(this.projectName, this.taskName, id).subscribe({
      next: () => {
        this.makingDefaultId.set(null);
        this.loadTriggers();
      },
      error: (error: unknown) => {
        this.makingDefaultId.set(null);
        this.notify.failure("Couldn't make trigger the default", error);
      },
    });
  }

  fireNow(id: string): void {
    this.firingId.set(id);
    this.fireSuccessId.set(null);
    this.triggersService.fireNow(this.projectName, this.taskName, id).subscribe({
      next: () => {
        this.firingId.set(null);
        this.fireSuccessId.set(id);
        this.loadTriggers();
        setTimeout(() => {
          if (this.fireSuccessId() === id) this.fireSuccessId.set(null);
        }, 3000);
      },
      error: (error: unknown) => {
        this.firingId.set(null);
        this.notify.failure('Failed to fire trigger', error);
      },
    });
  }

  configSummary(trigger: TriggerOut): string {
    const cfg = trigger.config as any;
    switch (trigger.type) {
      case 'polling': {
        let summary = `every ${this.formatSeconds(cfg.interval_secs ?? 300)}`;
        if (cfg.branch) summary += `, branch ${cfg.branch}`;
        return summary;
      }
      case 'reporter_push': {
        const parts: string[] = [];
        if (cfg.branches?.length) parts.push(`branches: ${cfg.branches.join(', ')}`);
        if (cfg.tags?.length) parts.push(`tags: ${cfg.tags.join(', ')}`);
        if (cfg.releases_only) parts.push('releases only');
        if (cfg.include_flake_inputs) parts.push('flake input');
        return parts.join(' / ');
      }
      case 'reporter_pull_request': {
        const parts: string[] = [];
        if (cfg.branches?.length) parts.push(`branches: ${cfg.branches.join(', ')}`);
        if (cfg.actions?.length) parts.push(`actions: ${cfg.actions.join(', ')}`);
        if (cfg.require_approval === false) parts.push('approval gate off');
        return parts.join(' / ');
      }
      case 'time': {
        const cron = cfg.cron ?? '';
        const description = cronExpressionToPlainEnglish(cron);
        const schedule = description ? `${description} (${cron})` : cron;
        return cfg.branch ? `${schedule}, branch ${cfg.branch}` : schedule;
      }
    }
  }

  private formatSeconds(secs: number): string {
    if (secs < 60) return `${secs}s`;
    const m = Math.floor(secs / 60);
    if (m < 60) return `${m} min`;
    return `${Math.floor(m / 60)}h ${m % 60}min`;
  }

}
