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
  Action,
  ActionConfig,
  ActionType,
  CreateActionRequest,
  GIT_HOST_STATUS_EVENTS,
  PrGenerator,
  PrGranularity,
  PrVerifyGate,
  UpdateActionRequest,
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
  existing = input<Action | null>(null);
  outboundIntegrations = input<IntegrationOption[]>([]);
  open = input<boolean>(false);
  error = input<string | null>(null);

  saved = output<CreateActionRequest | UpdateActionRequest>();
  closed = output<void>();

  visible = signal(false);
  type = signal<ActionType>('send_mail');
  name = signal('');
  active = signal(true);
  events = signal<string[]>([]);
  readonly taskFamilies = ['build', 'evaluation', 'task'];
  recipientsRaw = signal('');
  subjectTemplate = signal('');
  url = signal('');
  tokenValue = signal('');
  homeserver = signal('');
  roomId = signal('');
  secretValue = signal('');
  integrationId = signal('');
  prGenerator = signal<PrGenerator>('flake_lock');
  prGranularity = signal<PrGranularity>('per_run');
  prVerifyGate = signal<PrVerifyGate>('build');
  prBranchPattern = signal('gradient/flake-lock-update');
  prTitleTemplate = signal('');
  prBodyTemplate = signal('');
  prUpdateExisting = signal(true);

  readonly generatorOptions: { label: string; value: PrGenerator }[] = [
    { label: 'Flake Lock', value: 'flake_lock' },
  ];

  readonly granularityOptions: { label: string; value: PrGranularity }[] = [
    { label: 'Per Run', value: 'per_run' },
    { label: 'Per Input', value: 'per_input' },
  ];

  readonly verifyGateOptions: { label: string; value: PrVerifyGate }[] = [
    { label: 'None', value: 'none' },
    { label: 'Eval', value: 'eval' },
    { label: 'Build', value: 'build' },
  ];

  readonly smtpEnabled = computed(() => this.config.smtpEnabled);

  readonly typeOptions = computed(() => {
    const opts: { label: string; value: ActionType }[] = [];
    if (this.smtpEnabled()) opts.push({ label: 'Send Mail', value: 'send_mail' });
    opts.push({ label: 'Send Web Request', value: 'send_web_request' });
    opts.push({ label: 'Send Matrix Message', value: 'send_matrix_message' });
    opts.push({ label: 'Send Slack Message', value: 'send_slack_message' });
    opts.push({ label: 'Git Host Status Report', value: 'git_host_status_report' });
    opts.push({ label: 'Open PR', value: 'open_pr' });
    return opts;
  });

  readonly integrationOptions = computed(() =>
    this.outboundIntegrations().map((i) => ({ label: i.display_name, value: i.id })),
  );

  readonly displayedEvents = computed(() =>
    this.type() === 'git_host_status_report' ? GIT_HOST_STATUS_EVENTS : this.events(),
  );

  // `git_host_status_report` and `open_pr` fire on an internal gate, not
  // user-selected events: the status reporter tracks the whole lifecycle, and
  // Open PR fires when an input_update evaluation passes its verify gate.
  readonly eventsHardwired = computed(
    () => this.type() === 'git_host_status_report' || this.type() === 'open_pr',
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
      this.type.set(cur.action_type);
      this.events.set([...cur.events]);
      this.applyConfigToForm(cur.config);
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
    this.resetPrFields();
  }

  private resetPrFields(): void {
    this.prGenerator.set('flake_lock');
    this.prGranularity.set('per_run');
    this.prVerifyGate.set('build');
    this.prBranchPattern.set('gradient/flake-lock-update');
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
        this.prGenerator.set(cfg.generator);
        this.prGranularity.set(cfg.granularity);
        this.prVerifyGate.set(cfg.verify_gate);
        this.prBranchPattern.set(cfg.branch_pattern);
        this.prTitleTemplate.set(cfg.title_template ?? '');
        this.prBodyTemplate.set(cfg.body_template ?? '');
        this.prUpdateExisting.set(cfg.update_existing);
        break;
      case 'send_matrix_message':
        this.homeserver.set(cfg.homeserver);
        this.roomId.set(cfg.room_id);
        break;
      case 'send_slack_message':
        break;
    }
  }

  onTypeChange(newType: ActionType): void {
    this.type.set(newType);
    this.resetConfigFields();
    if (newType === 'git_host_status_report') this.events.set([...GIT_HOST_STATUS_EVENTS]);
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
          granularity: this.prGranularity(),
          verify_gate: this.prVerifyGate(),
          branch_pattern: this.prBranchPattern().trim(),
          update_existing: this.prUpdateExisting(),
        };
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
