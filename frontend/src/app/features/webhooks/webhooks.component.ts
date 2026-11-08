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
import { WebhookScopeRef, WebhookResponse, DeliveryListItem_WebhookDeliveryId, accessTo } from '@core/models';
import { ProjectAccessData } from '@core/resolvers/project-access.resolver';
import { CacheAccessData } from '@core/resolvers/cache-access.resolver';
import { canManageTeam, createMissingPermissionLoadStateError, createNoAccessLoadStateError } from '@shared/access';
import { TeamAccessData } from '@core/resolvers/team-access.resolver';
import { AuthService } from '@core/services/auth.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { WebhooksService } from '@core/services/webhooks.service';
import { apiErrorMessage } from '@core/services/api.service';
import { NotifyService } from '@core/services/notify.service';
import {
  BadgeComponent,
  ButtonComponent,
  CheckboxComponent,
  CopyFieldComponent,
  type Crumb,
  DialogComponent,
  FormFieldComponent,
  InputDirective,
  LoadStateComponent,
  MissingPermissionLoadStateError,
  PageLayoutComponent,
  SubmitDirective,
  TableComponent,
} from '@gradient/ui/ui';
import { LabelHelpComponent } from '@shared/ui';
import { ConfirmService } from '@shared/confirm';
import { relativeTime, ServerTimestampToLocalDateTimePipe } from '@shared/text';
import { fieldChecks } from '@shared/forms';
import { ActionEventsComponent } from '../tasks/task-actions/action-events.component';
import { links } from '@shared/routing';

@Component({
  selector: 'app-webhooks',
  standalone: true,
  imports: [
    CopyFieldComponent,
    LabelHelpComponent,
    CommonModule,
    ServerTimestampToLocalDateTimePipe,
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
    SubmitDirective,
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
  private confirm = inject(ConfirmService);
  private auth = inject(AuthService);

  scope!: WebhookScopeRef;
  title = '';
  breadcrumb = computed(() => this.trail());

  loading = signal(true);
  loadError = signal<unknown>(null);
  denied = signal<MissingPermissionLoadStateError | null>(null);
  saving = signal(false);
  formError = signal<string | null>(null);
  togglingId = signal<string | null>(null);
  webhooks = signal<WebhookResponse[]>([]);

  editing = signal<WebhookResponse | null>(null);
  formOpen = signal(false);
  name = signal('');
  url = signal('');
  events = signal<string[]>([]);
  globs = signal('');
  active = signal(true);
  attempted = signal(false);
  nameError = computed(() => (this.attempted() && !this.name().trim() ? 'Enter a name.' : null));
  urlError = computed(() => {
    if (!this.url().trim()) return this.attempted() ? 'Enter the URL to receive events.' : null;
    return fieldChecks.httpUrl(this.url());
  });

  revealedSecret = signal<string | null>(null);
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
        this.title = 'Project event notifications';
        break;
      case 'cache':
        this.scope = { kind: 'cache', name: paramMap.get('cache') ?? '' };
        this.title = 'Cache event notifications';
        break;
      case 'team':
        this.scope = { kind: 'team', name: paramMap.get('team') ?? '' };
        this.title = 'Team event notifications';
        break;
      default:
        this.scope = { kind: 'instance' };
        this.title = 'Instance event notifications';
    }
    this.denied.set(this.missingPermission());
    this.load();
  }

  private trail(): Crumb[] {
    const page = { label: 'Event notifications' };
    switch (this.scope.kind) {
      case 'project':
        return this.crumbs.projectSettings(this.scope.name, page);
      case 'cache':
        return this.crumbs.cacheSettings(this.scope.name, page);
      case 'team':
        return this.crumbs.team(this.scope.name, { label: 'Settings', link: links.teamPage(this.scope.name, 'settings') }, page);
      case 'instance':
        return [{ label: 'Job Board', link: links.board('health') }, page];
    }
  }

  private missingPermission(): MissingPermissionLoadStateError | null {
    if (this.scope.kind === 'project') {
      const project = (this.route.snapshot.data['projectAccess'] as ProjectAccessData | undefined)?.project;
      return accessTo(project, 'manageWebhooks').canEdit ? null : createMissingPermissionLoadStateError('manageWebhooks', 'project');
    }
    if (this.scope.kind === 'cache') {
      const cache = (this.route.parent?.snapshot.data['cacheAccess'] as CacheAccessData | undefined)?.cache;
      return accessTo(cache, 'manageCacheWebhooks').canEdit ? null : createMissingPermissionLoadStateError('manageCacheWebhooks', 'cache');
    }
    if (this.scope.kind === 'team') {
      const team = (this.route.snapshot.data['teamAccess'] as TeamAccessData | undefined)?.team;
      return canManageTeam(team, this.auth.user()) ? null : createNoAccessLoadStateError('team', 'admins');
    }
    return null;
  }

  load(): void {
    if (this.denied()) {
      this.loading.set(false);
      return;
    }
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
    this.attempted.set(false);
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
    if (this.saving()) return;
    this.attempted.set(true);
    if (this.nameError() || this.urlError()) return;
    const body = { name: this.name().trim(), url: this.url().trim(), events: this.patterns(), active: this.active() };
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
          this.notify.success('Event notification saved');
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
          this.notify.success(active ? 'Event notification enabled' : 'Event notification disabled');
          this.load();
        },
        error: (err: unknown) => this.notify.failure(active ? "Couldn't enable event notification" : "Couldn't disable event notification", err),
      });
  }

  rotate(hook: WebhookResponse): void {
    this.confirm
      .confirm({
        title: 'Rotate event notification secret',
        target: hook.name,
        consequence: 'The current secret can no longer verify deliveries. Pass the new secret to the receiving service.',
        confirmLabel: 'Rotate',
        failure: "Couldn't rotate event notification secret",
        action: () => this.service.rotateSecret(this.scope, hook.id),
      })
      .subscribe((res) => this.revealedSecret.set(res.secret));
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
    this.confirm
      .confirm({
        title: 'Delete event notification',
        target: hook.name,
        consequence: 'Deletes the event notification and its delivery history. The deletion is permanent.',
        confirmLabel: 'Delete',
        failure: "Can't delete webhook.",
        action: () => this.service.delete(this.scope, hook.id),
      })
      .subscribe(() => {
        this.notify.success('Event notification deleted');
        this.load();
      });
  }
}
