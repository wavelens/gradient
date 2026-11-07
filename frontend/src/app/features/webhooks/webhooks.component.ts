/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { ActivatedRoute } from '@angular/router';
import { finalize } from 'rxjs';
import { WebhookScopeRef, WebhookResponse, DeliveryListItem_WebhookDeliveryId } from '@core/models';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { WebhooksService } from '@core/services/webhooks.service';
import { apiErrorMessage } from '@core/services/api.service';
import { NotifyService } from '@core/services/notify.service';
import {
  BadgeComponent,
  ButtonComponent,
  CheckboxComponent,
  type Crumb,
  DialogComponent,
  FormFieldComponent,
  InputDirective,
  LoadStateComponent,
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
    FormFieldComponent,
    InputDirective,
    LoadStateComponent,
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
  private crumbs = inject(BreadcrumbsService);
  private notify = inject(NotifyService);

  scope!: WebhookScopeRef;
  title = '';
  breadcrumb = computed(() => this.trail());

  loading = signal(true);
  loadError = signal<unknown>(null);
  saving = signal(false);
  formError = signal<string | null>(null);
  deleteError = signal<string | null>(null);
  deleting = signal(false);
  togglingId = signal<string | null>(null);
  rotatingId = signal<string | null>(null);
  webhooks = signal<WebhookResponse[]>([]);

  editing = signal<WebhookResponse | null>(null);
  formOpen = signal(false);
  name = signal('');
  url = signal('');
  events = signal<string[]>([]);
  globs = signal('');
  active = signal(true);

  revealedSecret = signal<string | null>(null);
  confirmDeleteId = signal<string | null>(null);
  testResult = signal<{ id: string; delivery: DeliveryListItem_WebhookDeliveryId | null } | null>(null);
  deliveriesFor = signal<WebhookResponse | null>(null);
  deliveries = signal<DeliveryListItem_WebhookDeliveryId[]>([]);
  deliveriesLoading = signal(false);
  deliveriesError = signal<unknown>(null);

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
      case 'team':
        this.scope = { kind: 'team', name: paramMap.get('team') ?? '' };
        this.title = 'Team Webhooks';
        break;
      default:
        this.scope = { kind: 'instance' };
        this.title = 'Instance Webhooks';
    }
    this.load();
  }

  private trail(): Crumb[] {
    const page = { label: 'Webhooks' };
    switch (this.scope.kind) {
      case 'project':
        return this.crumbs.projectSettings(this.scope.name, page);
      case 'cache':
        return this.crumbs.cacheSettings(this.scope.name, page);
      case 'team':
        return this.crumbs.team(this.scope.name, { label: 'Settings', link: ['/team', this.scope.name, 'settings'] }, page);
      case 'instance':
        return [{ label: 'Job Board', link: ['/board', 'health'] }, page];
    }
  }

  load(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.service.list(this.scope).subscribe({
      next: (list) => {
        this.webhooks.set(list);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  startCreate(): void {
    this.fill(null);
  }

  startEdit(hook: WebhookResponse): void {
    this.fill(hook);
  }

  private fill(hook: WebhookResponse | null): void {
    this.formError.set(null);
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
    this.formError.set(null);
    const done = () => {
      this.saving.set(false);
      this.formOpen.set(false);
      this.load();
    };
    const failed = (err: unknown) => {
      this.formError.set(apiErrorMessage(err, 'Failed to save webhook.'));
      this.saving.set(false);
    };
    if (target) {
      this.service.update(this.scope, target.id, body).subscribe({
        next: () => {
          done();
          this.notify.success('Webhook saved');
        },
        error: failed,
      });
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

  toggleActive(hook: WebhookResponse): void {
    const active = !hook.active;
    this.togglingId.set(hook.id);
    this.service
      .update(this.scope, hook.id, { active })
      .pipe(finalize(() => this.togglingId.set(null)))
      .subscribe({
        next: () => {
          this.notify.success(active ? 'Webhook enabled' : 'Webhook disabled');
          this.load();
        },
        error: (err: unknown) => this.notify.failure(active ? "Can't enable webhook" : "Can't disable webhook", err),
      });
  }

  rotate(hook: WebhookResponse): void {
    this.rotatingId.set(hook.id);
    this.service
      .rotateSecret(this.scope, hook.id)
      .pipe(finalize(() => this.rotatingId.set(null)))
      .subscribe({
        next: (res) => this.revealedSecret.set(res.secret),
        error: (err: unknown) => this.notify.failure("Can't rotate webhook secret", err),
      });
  }

  testing(hook: WebhookResponse): boolean {
    const result = this.testResult();
    return result?.id === hook.id && result.delivery === null;
  }

  test(hook: WebhookResponse): void {
    this.testResult.set({ id: hook.id, delivery: null });
    this.service.test(this.scope, hook.id).subscribe({
      next: (delivery) => this.testResult.set({ id: hook.id, delivery }),
      error: (err: unknown) => {
        this.testResult.set(null);
        this.notify.failure("Can't send test delivery", err);
      },
    });
  }

  openDeliveries(hook: WebhookResponse): void {
    this.deliveriesFor.set(hook);
    this.deliveries.set([]);
    this.loadDeliveries();
  }

  loadDeliveries(): void {
    const hook = this.deliveriesFor();
    if (!hook) return;
    this.deliveriesLoading.set(true);
    this.deliveriesError.set(null);
    this.service.deliveries(this.scope, hook.id).subscribe({
      next: (list) => {
        if (this.deliveriesFor() !== hook) return;
        this.deliveries.set(list);
        this.deliveriesLoading.set(false);
      },
      error: (error: unknown) => {
        if (this.deliveriesFor() !== hook) return;
        this.deliveriesError.set(error);
        this.deliveriesLoading.set(false);
      },
    });
  }

  startDelete(hook: WebhookResponse): void {
    this.deleteError.set(null);
    this.confirmDeleteId.set(hook.id);
  }

  confirmDelete(): void {
    const id = this.confirmDeleteId();
    if (!id || this.deleting()) return;
    this.deleting.set(true);
    this.deleteError.set(null);
    this.service.delete(this.scope, id).subscribe({
      next: () => {
        this.deleting.set(false);
        this.confirmDeleteId.set(null);
        this.notify.success('Webhook deleted');
        this.load();
      },
      error: (err: unknown) => {
        this.deleting.set(false);
        this.deleteError.set(apiErrorMessage(err, "Can't delete webhook."));
      },
    });
  }
}
