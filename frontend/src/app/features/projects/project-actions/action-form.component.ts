/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnChanges, SimpleChanges, computed, inject, input, output, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { DialogModule } from 'primeng/dialog';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';
import { SelectModule } from 'primeng/select';
import { CheckboxModule } from 'primeng/checkbox';
import { ConfigService } from '@core/services/config.service';
import {
  Action,
  ActionConfig,
  ActionType,
  CreateActionRequest,
  FORGE_STATUS_EVENTS,
  UpdateActionRequest,
} from '@core/models';
import { ActionEventsComponent } from './action-events.component';

type FormMode = 'create' | 'edit';

interface IntegrationOption {
  id: string;
  display_name: string;
}

@Component({
  selector: 'app-action-form',
  standalone: true,
  imports: [
    CommonModule,
    FormsModule,
    DialogModule,
    ButtonModule,
    InputTextModule,
    SelectModule,
    CheckboxModule,
    ActionEventsComponent,
  ],
  templateUrl: './action-form.component.html',
  styleUrl: './action-form.component.scss',
})
export class ActionFormComponent implements OnChanges {
  private config = inject(ConfigService);

  mode = input<FormMode>('create');
  existing = input<Action | null>(null);
  outboundIntegrations = input<IntegrationOption[]>([]);
  open = input<boolean>(false);

  saved = output<CreateActionRequest | UpdateActionRequest>();
  closed = output<void>();

  visible = signal(false);
  type = signal<ActionType>('send_mail');
  name = signal('');
  active = signal(true);
  events = signal<string[]>([]);
  recipientsRaw = signal('');
  subjectTemplate = signal('');
  url = signal('');
  tokenValue = signal('');
  integrationId = signal('');

  readonly smtpEnabled = computed(() => this.config.smtpEnabled);

  readonly typeOptions = computed(() => {
    const opts: { label: string; value: ActionType }[] = [];
    if (this.smtpEnabled()) opts.push({ label: 'Send Mail', value: 'send_mail' });
    opts.push({ label: 'Send Web Request', value: 'send_web_request' });
    opts.push({ label: 'Forge Status Report', value: 'forge_status_report' });
    return opts;
  });

  readonly integrationOptions = computed(() =>
    this.outboundIntegrations().map((i) => ({ label: i.display_name, value: i.id })),
  );

  readonly displayedEvents = computed(() =>
    this.type() === 'forge_status_report' ? FORGE_STATUS_EVENTS : this.events(),
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
    } else {
      this.name.set('');
      this.active.set(true);
      this.type.set(this.smtpEnabled() ? 'send_mail' : 'send_web_request');
      this.events.set([]);
      this.recipientsRaw.set('');
      this.subjectTemplate.set('');
      this.url.set('');
      this.tokenValue.set('');
      this.integrationId.set('');
    }
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
      case 'forge_status_report':
        this.integrationId.set(cfg.integration_id);
        break;
    }
  }

  onTypeChange(newType: ActionType): void {
    this.type.set(newType);
    this.recipientsRaw.set('');
    this.subjectTemplate.set('');
    this.url.set('');
    this.tokenValue.set('');
    this.integrationId.set('');
    if (newType === 'forge_status_report') this.events.set([...FORGE_STATUS_EVENTS]);
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
      case 'forge_status_report':
        return { type: 'forge_status_report', integration_id: this.integrationId() };
    }
  }

  onSubmit(): void {
    const config = this.buildConfig();
    const events =
      this.type() === 'forge_status_report' ? [...FORGE_STATUS_EVENTS] : this.events();
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
