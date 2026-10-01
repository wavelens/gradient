/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { ActivatedRoute } from '@angular/router';
import { Webhook, WebhookDelivery, WebhookScopeRef } from '@core/models';
import { WebhooksService } from '@core/services/webhooks.service';
import {
  BadgeComponent,
  ButtonComponent,
  CheckboxComponent,
  DialogComponent,
  EmptyStateComponent,
  FormFieldComponent,
  IconComponent,
  InputDirective,
  LoadingSpinnerComponent,
  PageLayoutComponent,
  TableComponent,
} from '@gradient/ui/ui';
import { LabelHelpComponent } from '@shared/ui';
import { relativeTime } from '@shared/text';
import { ActionEventsComponent } from '../tasks/task-actions/action-events.component';

@Component({
  selector: 'app-webhooks',
  standalone: true,
  imports: [
    LabelHelpComponent,
    CommonModule,
    FormsModule,
    ActionEventsComponent,
    BadgeComponent,
    ButtonComponent,
    CheckboxComponent,
    DialogComponent,
    EmptyStateComponent,
    FormFieldComponent,
    IconComponent,
    InputDirective,
    LoadingSpinnerComponent,
    PageLayoutComponent,
    TableComponent,
  ],
  templateUrl: './webhooks.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './webhooks.component.scss',
})
export class WebhooksComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private service = inject(WebhooksService);

  scope!: WebhookScopeRef;
  title = '';

  loading = signal(true);
  saving = signal(false);
  error = signal<string | null>(null);
  webhooks = signal<Webhook[]>([]);

  editing = signal<Webhook | null>(null);
  formOpen = signal(false);
  name = signal('');
  url = signal('');
  events = signal<string[]>([]);
  globs = signal('');
  active = signal(true);

  revealedSecret = signal<string | null>(null);
  confirmDeleteId = signal<string | null>(null);
  testResult = signal<{ id: string; delivery: WebhookDelivery | null } | null>(null);
  deliveriesFor = signal<Webhook | null>(null);
  deliveries = signal<WebhookDelivery[]>([]);

  relativeTime = relativeTime;

  ngOnInit(): void {
    const { data, paramMap } = this.route.snapshot;
    switch (data['webhookScope']) {
      case 'project':
        this.scope = { kind: 'project', name: paramMap.get('project') ?? '' };
        this.title = 'Project Webhooks';
        break;
      case 'cache':
        this.scope = { kind: 'cache', name: paramMap.get('cache') ?? '' };
        this.title = 'Cache Webhooks';
        break;
      default:
        this.scope = { kind: 'instance' };
        this.title = 'Instance Webhooks';
    }
    this.load();
  }

  load(): void {
    this.loading.set(true);
    this.service.list(this.scope).subscribe({
      next: (list) => {
        this.webhooks.set(list);
        this.loading.set(false);
      },
      error: (err) => {
        this.error.set(err?.message || 'Failed to load webhooks.');
        this.loading.set(false);
      },
    });
  }

  startCreate(): void {
    this.fill(null);
  }

  startEdit(hook: Webhook): void {
    this.fill(hook);
  }

  private fill(hook: Webhook | null): void {
    this.error.set(null);
    this.editing.set(hook);
    this.name.set(hook?.name ?? '');
    this.url.set(hook?.url ?? '');
    const patterns = hook?.events ?? [];
    this.events.set(patterns.filter((e) => !e.includes('*')));
    this.globs.set(patterns.filter((e) => e.includes('*')).join(', '));
    this.active.set(hook?.active ?? true);
    this.formOpen.set(true);
  }

  private patterns(): string[] {
    const typed = this.globs()
      .split(',')
      .map((g) => g.trim())
      .filter((g) => g.length > 0);
    return [...this.events(), ...typed];
  }

  save(): void {
    const body = { name: this.name(), url: this.url(), events: this.patterns(), active: this.active() };
    const target = this.editing();
    this.saving.set(true);
    this.error.set(null);
    const done = () => {
      this.saving.set(false);
      this.formOpen.set(false);
      this.load();
    };
    const failed = (err: { message?: string }) => {
      this.error.set(err?.message || 'Failed to save webhook.');
      this.saving.set(false);
    };
    if (target) {
      this.service.update(this.scope, target.id, body).subscribe({ next: done, error: failed });
    } else {
      this.service.create(this.scope, body).subscribe({
        next: (res) => {
          this.revealedSecret.set(res.secret);
          done();
        },
        error: failed,
      });
    }
  }

  toggleActive(hook: Webhook): void {
    this.service.update(this.scope, hook.id, { active: !hook.active }).subscribe(() => this.load());
  }

  rotate(hook: Webhook): void {
    this.service.rotateSecret(this.scope, hook.id).subscribe((res) => this.revealedSecret.set(res.secret));
  }

  test(hook: Webhook): void {
    this.testResult.set({ id: hook.id, delivery: null });
    this.service.test(this.scope, hook.id).subscribe({
      next: (delivery) => this.testResult.set({ id: hook.id, delivery }),
      error: (err) => {
        this.testResult.set(null);
        this.error.set(err?.message || 'Test delivery failed.');
      },
    });
  }

  openDeliveries(hook: Webhook): void {
    this.deliveriesFor.set(hook);
    this.deliveries.set([]);
    this.service.deliveries(this.scope, hook.id).subscribe((list) => this.deliveries.set(list));
  }

  confirmDelete(): void {
    const id = this.confirmDeleteId();
    if (!id) return;
    this.service.delete(this.scope, id).subscribe(() => {
      this.confirmDeleteId.set(null);
      this.load();
    });
  }
}
