/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnChanges, SimpleChanges, computed, inject, input, output, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { ConfigService } from '@core/services/config.service';
import {
  ACTION_TYPE_LABEL,
  ActionType,
  GIT_HOST_STATUS_EVENTS,
  ActionResponse,
  CreateActionRequest,
  UpdateActionRequest,
  PatchGeneratorKind,
  PrGranularity,
  VerifyGate,
  ActionConfig,
} from '@core/models';
import { ActionEventsComponent } from './action-events.component';
import {
  ButtonComponent,
  CheckboxComponent,
  DialogComponent,
  FormFieldComponent,
  IconComponent,
  InputDirective,
  SelectButtonComponent,
  SelectComponent,
} from '@gradient/ui/ui';
import { LabelHelpComponent } from '@shared/ui';

type FormMode = 'create' | 'edit';

interface IntegrationOption {
  id: string;
  display_name: string;
}

interface TaskOption {
  id: string;
  name: string;
}

const DEFAULT_BRANCH_PATTERN: Record<PatchGeneratorKind, string> = {
  flake_lock: 'gradient/flake-lock-update',
  exec_tree: 'gradient/{entry_point}',
};

@Component({
  selector: 'app-action-form',
  standalone: true,
  imports: [
    LabelHelpComponent,
    CommonModule,
    FormsModule,
    DialogComponent,
    ButtonComponent,
    InputDirective,
    SelectComponent,
    SelectButtonComponent,
    CheckboxComponent,
    InputDirective,
    ActionEventsComponent,
    IconComponent,
    FormFieldComponent,
  ],
  templateUrl: './action-form.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './action-form.component.scss',
})
export class ActionFormComponent implements OnChanges {
  private config = inject(ConfigService);

  mode = input<FormMode>('create');
  existing = input<ActionResponse | null>(null);
  outboundIntegrations = input<IntegrationOption[]>([]);
  tasks = input<TaskOption[]>([]);
  open = input<boolean>(false);
  error = input<string | null>(null);

  saved = output<CreateActionRequest | UpdateActionRequest>();
  closed = output<void>();

  visible = signal(false);
  type = signal<ActionType>('send_mail');
  name = signal('');
  active = signal(true);
  events = signal<string[]>([]);
  readonly taskFamilies = ['build', 'evaluation', 'exec_job', 'task'];
  recipientsRaw = signal('');
  teams = input<string[]>([]);
  teamOptions = computed(() => this.teams().map((team) => ({ label: team, value: team })));

  addTeamRecipient(team: string | null): void {
    if (!team) return;
    const current = this.recipientsRaw().trim();
    const entry = `team:${team}`;
    this.recipientsRaw.set(current ? `${current}, ${entry}` : entry);
  }
  subjectTemplate = signal('');
  url = signal('');
  tokenValue = signal('');
  homeserver = signal('');
  roomId = signal('');
  secretValue = signal('');
  integrationId = signal('');
  prGenerator = signal<PatchGeneratorKind>('flake_lock');
  prGranularity = signal<PrGranularity>('per_run');
  prVerifyGate = signal<VerifyGate>('build');
  prBranchPattern = signal(DEFAULT_BRANCH_PATTERN.flake_lock);
  prTitleTemplate = signal('');
  prBodyTemplate = signal('');
  prUpdateExisting = signal(true);
  startTask = signal('');

  readonly generatorOptions: { label: string; value: PatchGeneratorKind }[] = [
    { label: 'Flake Lock', value: 'flake_lock' },
    { label: 'Exec Tree', value: 'exec_tree' },
  ];

  readonly granularityOptions: { label: string; value: PrGranularity }[] = [
    { label: 'Per Run', value: 'per_run' },
    { label: 'Per Input', value: 'per_input' },
  ];

  readonly verifyGateOptions: { label: string; value: VerifyGate }[] = [
    { label: 'None', value: 'none' },
    { label: 'Eval', value: 'eval' },
    { label: 'Build', value: 'build' },
  ];

  readonly smtpEnabled = computed(() => this.config.smtpEnabled);

  readonly typeOptions = computed(() =>
    (
      [
        'send_mail',
        'send_web_request',
        'send_matrix_message',
        'send_slack_message',
        'git_host_status_report',
        'open_pr',
        'start_evaluation',
      ] as ActionType[]
    )
      .filter((value) => value !== 'send_mail' || this.smtpEnabled())
      .map((value) => ({ label: ACTION_TYPE_LABEL[value], value })),
  );

  readonly taskOptions = computed(() => this.tasks().map((t) => ({ label: t.name, value: t.id })));

  readonly integrationOptions = computed(() =>
    this.outboundIntegrations().map((i) => ({ label: i.display_name, value: i.id })),
  );

  readonly displayedEvents = computed(() =>
    this.type() === 'git_host_status_report' ? GIT_HOST_STATUS_EVENTS : this.events(),
  );

  // `git_host_status_report` and `open_pr` fire on an internal gate, not
  // user-selected events: the status reporter tracks the whole lifecycle, and
  // a PR action fires on the gate of its generator.
  readonly eventsHardwired = computed(
    () => this.type() === 'git_host_status_report' || this.type() === 'open_pr',
  );

  readonly defaultBranchPattern = computed(() => DEFAULT_BRANCH_PATTERN[this.prGenerator()]);

  readonly prEventsHint = computed(() =>
    this.prGenerator() === 'exec_tree'
      ? 'Fires when an exec job of this task completes with a changed tree.'
      : 'A PR action fires automatically when an input_update evaluation passes its verify gate. No event triggers apply.',
  );

  readonly typeRadioDisabled = computed(() => this.mode() === 'edit');

  ngOnChanges(changes: SimpleChanges): void {
    if (changes['open']) this.visible.set(this.open());
    if (changes['existing'] || changes['mode']) this.resetFromInputs();
  }

  private resetFromInputs(): void {
    const cur = this.existing();
    if (this.mode() === 'edit' && cur) {
      this.name.set(cur.name);
      this.active.set(cur.active);
      const config = cur.config as ActionConfig;
      this.type.set(config.type);
      this.events.set([...cur.events]);
      this.applyConfigToForm(config);
      this.tokenValue.set('');
      this.secretValue.set('');
    } else {
      this.name.set('');
      this.active.set(true);
      this.type.set(this.smtpEnabled() ? 'send_mail' : 'send_web_request');
      this.events.set([]);
      this.resetConfigFields();
    }
  }

  private resetConfigFields(): void {
    this.recipientsRaw.set('');
    this.subjectTemplate.set('');
    this.url.set('');
    this.tokenValue.set('');
    this.homeserver.set('');
    this.roomId.set('');
    this.secretValue.set('');
    this.integrationId.set('');
    this.startTask.set('');
    this.resetPrFields();
  }

  private resetPrFields(): void {
    this.prGenerator.set('flake_lock');
    this.prGranularity.set('per_run');
    this.prVerifyGate.set('build');
    this.prBranchPattern.set(DEFAULT_BRANCH_PATTERN.flake_lock);
    this.prTitleTemplate.set('');
    this.prBodyTemplate.set('');
    this.prUpdateExisting.set(true);
  }

  private applyConfigToForm(cfg: ActionConfig): void {
    switch (cfg.type) {
      case 'send_mail':
        this.recipientsRaw.set(cfg.recipients.join(', '));
        this.subjectTemplate.set(cfg.subject_template ?? '');
        break;
      case 'send_web_request':
        this.url.set(cfg.url);
        break;
      case 'git_host_status_report':
        this.integrationId.set(cfg.integration_id);
        break;
      case 'open_pr':
        this.integrationId.set(cfg.integration_id);
        this.resetPrFields();
        this.prGenerator.set(cfg.generator ?? this.prGenerator());
        this.prGranularity.set(cfg.granularity ?? this.prGranularity());
        this.prVerifyGate.set(cfg.verify_gate ?? this.prVerifyGate());
        this.prBranchPattern.set(cfg.branch_pattern ?? this.prBranchPattern());
        this.prTitleTemplate.set(cfg.title_template ?? '');
        this.prBodyTemplate.set(cfg.body_template ?? '');
        this.prUpdateExisting.set(cfg.update_existing ?? this.prUpdateExisting());
        break;
      case 'send_matrix_message':
        this.homeserver.set(cfg.homeserver);
        this.roomId.set(cfg.room_id);
        break;
      case 'send_slack_message':
        break;
      case 'start_evaluation':
        this.startTask.set(cfg.task);
        break;
    }
  }

  onTypeChange(newType: ActionType): void {
    this.type.set(newType);
    this.resetConfigFields();
    if (newType === 'git_host_status_report') this.events.set([...GIT_HOST_STATUS_EVENTS]);
  }

  onGeneratorChange(generator: PatchGeneratorKind): void {
    if (this.prBranchPattern() === DEFAULT_BRANCH_PATTERN[this.prGenerator()]) {
      this.prBranchPattern.set(DEFAULT_BRANCH_PATTERN[generator]);
    }
    this.prGenerator.set(generator);
  }

  generateToken(): void {
    const bytes = new Uint8Array(32);
    crypto.getRandomValues(bytes);
    const b64 = btoa(String.fromCharCode(...bytes))
      .replace(/\+/g, '-')
      .replace(/\//g, '_')
      .replace(/=+$/, '');
    this.tokenValue.set('gat_' + b64);
  }

  private buildConfig(): ActionConfig {
    switch (this.type()) {
      case 'send_mail': {
        const recipients = this.recipientsRaw()
          .split(',')
          .map((s) => s.trim())
          .filter((s) => s.length > 0);
        const subject = this.subjectTemplate().trim();
        const cfg: Extract<ActionConfig, { type: 'send_mail' }> = { type: 'send_mail', recipients };
        if (subject) cfg.subject_template = subject;
        return cfg;
      }
      case 'send_web_request': {
        const cfg: Extract<ActionConfig, { type: 'send_web_request' }> = {
          type: 'send_web_request',
          url: this.url().trim(),
        };
        const token = this.tokenValue().trim();
        if (token) cfg.token = token;
        return cfg;
      }
      case 'git_host_status_report':
        return { type: 'git_host_status_report', integration_id: this.integrationId() };
      case 'open_pr': {
        const cfg: Extract<ActionConfig, { type: 'open_pr' }> = {
          type: 'open_pr',
          integration_id: this.integrationId(),
          generator: this.prGenerator(),
          branch_pattern: this.prBranchPattern().trim(),
          update_existing: this.prUpdateExisting(),
        };
        if (this.prGenerator() === 'flake_lock') {
          cfg.granularity = this.prGranularity();
          cfg.verify_gate = this.prVerifyGate();
        }
        const title = this.prTitleTemplate().trim();
        const body = this.prBodyTemplate().trim();
        if (title) cfg.title_template = title;
        if (body) cfg.body_template = body;
        return cfg;
      }
      case 'send_matrix_message': {
        const cfg: Extract<ActionConfig, { type: 'send_matrix_message' }> = {
          type: 'send_matrix_message',
          homeserver: this.homeserver().trim(),
          room_id: this.roomId().trim(),
        };
        const secret = this.secretValue().trim();
        if (secret) cfg.access_token = secret;
        return cfg;
      }
      case 'send_slack_message': {
        const cfg: Extract<ActionConfig, { type: 'send_slack_message' }> = { type: 'send_slack_message' };
        const secret = this.secretValue().trim();
        if (secret) cfg.webhook_url = secret;
        return cfg;
      }
      case 'start_evaluation':
        return { type: 'start_evaluation', task: this.startTask() };
    }
  }

  onSubmit(): void {
    const config = this.buildConfig();
    const events = this.eventsHardwired() ? [] : this.events();
    if (this.mode() === 'create') {
      const req: CreateActionRequest = {
        name: this.name().trim(),
        config,
        events,
        active: this.active(),
      };
      this.saved.emit(req);
    } else {
      const req: UpdateActionRequest = {
        name: this.name().trim(),
        config,
        events,
        active: this.active(),
      };
      this.saved.emit(req);
    }
  }

  onCancel(): void {
    this.visible.set(false);
    this.closed.emit();
  }

  onVisibleChange(v: boolean): void {
    this.visible.set(v);
    if (!v) this.closed.emit();
  }
}
