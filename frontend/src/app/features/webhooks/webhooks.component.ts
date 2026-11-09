/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, ElementRef, OnInit, computed, inject, signal, viewChild } from '@angular/core';
import { CommonModule } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { ActivatedRoute } from '@angular/router';
import { finalize } from 'rxjs';
import { WebhookScopeRef, WebhookResponse, DeliveryListItem_WebhookDeliveryId, accessTo, eventNameToReadableText, eventNamePrefixesByNotificationScope, eventNameMatchesAnyPrefix } from '@core/models';
import { ProjectAccessData } from '@core/resolvers/project-access.resolver';
import { CacheAccessData } from '@core/resolvers/cache-access.resolver';
import { canManageTeam, createMissingPermissionLoadStateError, createNoAccessLoadStateError, isViewerCacheMember, isViewerProjectMember } from '@shared/access';
import { TeamAccessData } from '@core/resolvers/team-access.resolver';
import { AuthService } from '@core/services/auth.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { WebhooksService } from '@core/services/webhooks.service';
import { ApiError, apiErrorMessage } from '@core/services/api.service';
import { NotifyService } from '@core/services/notify.service';
import {
  BadgeComponent,
  type BadgeSeverity,
  ButtonComponent,
  CheckboxComponent,
  CopyFieldComponent,
  type Crumb,
  DialogComponent,
  FormFieldComponent,
  InputDirective,
  LoadStateComponent,
  MenuComponent,
  type MenuItem,
  MissingPermissionLoadStateError,
  PageLayoutComponent,
  SubmitDirective,
} from '@gradient/ui/ui';
import { LabelHelpComponent } from '@shared/ui';
import { ConfirmService } from '@shared/confirm';
import { RelativeTimePipe } from '@shared/text';
import { OutgoingWebhookDeliveryListDialog, getDeliveryResultReason } from '@shared/outgoing-deliveries';
import { dialogFormChangesSinceOpen, fieldChecks, injectRevealFirstInvalid } from '@shared/forms';
import { ActionEventsComponent } from '../tasks/task-actions/action-events.component';
import { links } from '@shared/routing';

type WebhookListItemWithLastDeliveryAttempt = WebhookResponse & {
  last_delivery?: Pick<DeliveryListItem_WebhookDeliveryId, 'success' | 'delivered_at' | 'response_status' | 'error_message'> | null;
};

type WebhookFormField = 'name' | 'url' | 'globs';

function getWebhookFormFieldForServerError(err: unknown): WebhookFormField | null {
  if (!(err instanceof ApiError)) return null;
  if (err.status === 409) return 'name';
  if (err.status !== 422) return null;
  if (/\bURL\b/.test(err.message)) return 'url';
  return /event pattern/.test(err.message) ? 'globs' : null;
}

type WebhookCardState = 'active' | 'failing' | 'inactive';

const webhookCardStateBadgeLabelAndColorSeverityByState: Record<WebhookCardState, { label: string; severity: BadgeSeverity }> = {
  active: { label: 'Active', severity: 'success' },
  failing: { label: 'Failing', severity: 'danger' },
  inactive: { label: 'Inactive', severity: 'warning' },
};

@Component({
  selector: 'app-webhooks',
  standalone: true,
  imports: [
    RelativeTimePipe,
    CopyFieldComponent,
    LabelHelpComponent,
    CommonModule,
    FormsModule,
    ActionEventsComponent,
    OutgoingWebhookDeliveryListDialog,
    BadgeComponent,
    ButtonComponent,
    CheckboxComponent,
    DialogComponent,
    FormFieldComponent,
    InputDirective,
    LoadStateComponent,
    MenuComponent,
    PageLayoutComponent,
    SubmitDirective,
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
  eventNamePrefixes: readonly string[] | undefined;
  eventGlobsExamplesHint = '';
  title = '';
  breadcrumb = computed(() => this.trail());

  loading = signal(true);
  loadError = signal<unknown>(null);
  denied = signal<MissingPermissionLoadStateError | null>(null);
  saving = signal(false);
  formError = signal<string | null>(null);
  togglingId = signal<string | null>(null);
  webhooks = signal<WebhookListItemWithLastDeliveryAttempt[]>([]);

  editing = signal<WebhookResponse | null>(null);
  formOpen = signal(false);
  name = signal('');
  url = signal('');
  events = signal<string[]>([]);
  globs = signal('');
  active = signal(true);
  formChanges = dialogFormChangesSinceOpen(() => [this.name(), this.url(), this.events(), this.globs(), this.active()]);
  attempted = signal(false);
  serverFieldRejection = signal<{ field: WebhookFormField; value: string; message: string } | null>(null);
  nameError = computed(() => (this.attempted() && !this.name().trim() ? 'Enter a name.' : this.serverRejectionOf('name', this.name())));
  urlError = computed(() => {
    if (!this.url().trim()) return this.attempted() ? 'Enter the URL to receive events.' : null;
    return fieldChecks.httpUrl(this.url()) ?? this.serverRejectionOf('url', this.url());
  });
  globsError = computed(() => this.serverRejectionOf('globs', this.globs()));
  private webhookForm = viewChild<ElementRef<HTMLElement>>('webhookForm');
  private revealFirstInvalid = injectRevealFirstInvalid();

  revealedSecret = signal<string | null>(null);
  webhookIdWithTestDeliveryInFlight = signal<string | null>(null);
  deliveriesFor = signal<WebhookResponse | null>(null);
  openWebhookDeliveryHistoryLoadFunctions = computed(() => {
    const hook = this.deliveriesFor();
    if (!hook) return null;
    return {
      name: hook.name,
      page: (limit: number, offset: number) => this.service.deliveries(this.scope, hook.id, limit, offset),
      detail: (deliveryId: string) => this.service.delivery(this.scope, hook.id, deliveryId),
    };
  });

  overflowMenuItemsOfLastClickedWebhookRow = signal<MenuItem[]>([]);

  eventNameToReadableText = eventNameToReadableText;
  getDeliveryResultReason = getDeliveryResultReason;

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
    this.eventNamePrefixes = this.scope.kind === 'instance' ? undefined : eventNamePrefixesByNotificationScope[this.scope.kind];
    const examples = this.eventNamePrefixes ? this.eventNamePrefixes.slice(0, 2).map((prefix) => `${prefix}.*`) : ['build.*', 'cache.nar.*'];
    this.eventGlobsExamplesHint = 'Comma-separated, e.g. ' + examples.join(', ');
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
        return this.crumbs.admin(page);
    }
  }

  private missingPermission(): MissingPermissionLoadStateError | null {
    if (this.scope.kind === 'project') {
      const project = (this.route.snapshot.data['projectAccess'] as ProjectAccessData | undefined)?.project;
      return accessTo(project, 'manageWebhooks').canEdit ? null : createMissingPermissionLoadStateError('manageWebhooks', 'project', isViewerProjectMember(project));
    }
    if (this.scope.kind === 'cache') {
      const cache = (this.route.parent?.snapshot.data['cacheAccess'] as CacheAccessData | undefined)?.cache;
      return accessTo(cache, 'manageCacheWebhooks').canEdit ? null : createMissingPermissionLoadStateError('manageCacheWebhooks', 'cache', isViewerCacheMember(cache));
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

  getWebhookCardStatusChip(hook: WebhookListItemWithLastDeliveryAttempt): { label: string; severity: BadgeSeverity } {
    const state: WebhookCardState = !hook.active ? 'inactive' : hook.last_delivery?.success === false ? 'failing' : 'active';
    return webhookCardStateBadgeLabelAndColorSeverityByState[state];
  }

  openWebhookOverflowMenuAtButton(event: Event, hook: WebhookResponse, menu: MenuComponent): void {
    this.overflowMenuItemsOfLastClickedWebhookRow.set([
      { label: 'Rotate secret', icon: 'key', command: () => this.rotate(hook) },
      { label: hook.active ? 'Disable' : 'Enable', icon: 'power_settings_new', command: () => this.toggleActive(hook) },
      { label: 'Delete', icon: 'delete', danger: true, command: () => this.startDelete(hook) },
    ]);
    menu.toggle(event);
  }

  startCreate(): void {
    this.fill(null);
  }

  startEdit(hook: WebhookResponse): void {
    this.fill(hook);
  }

  private serverRejectionOf(field: WebhookFormField, value: string): string | null {
    const rejection = this.serverFieldRejection();
    return rejection?.field === field && rejection.value === value.trim() ? rejection.message : null;
  }

  private fill(hook: WebhookResponse | null): void {
    this.formError.set(null);
    this.serverFieldRejection.set(null);
    this.attempted.set(false);
    this.editing.set(hook);
    this.name.set(hook?.name ?? '');
    this.url.set(hook?.url ?? '');
    const prefixes = this.eventNamePrefixes;
    const offered = (event: string) => !event.includes('*') && (!prefixes || eventNameMatchesAnyPrefix(event, prefixes));
    const patterns = hook?.events ?? [];
    this.events.set(patterns.filter(offered));
    this.globs.set(patterns.filter((event) => !offered(event)).join(', '));
    this.active.set(hook?.active ?? true);
    this.formChanges.mark();
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
    if (this.nameError() || this.urlError() || this.globsError()) {
      this.revealFirstInvalid(() => this.webhookForm()?.nativeElement);
      return;
    }
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
      this.saving.set(false);
      const field = getWebhookFormFieldForServerError(err);
      if (!field || !(err instanceof ApiError)) {
        this.formError.set(apiErrorMessage(err, 'Failed to save webhook.'));
        return;
      }
      const value = { name: body.name, url: body.url, globs: this.globs().trim() }[field];
      this.serverFieldRejection.set({ field, value, message: err.message });
      this.revealFirstInvalid(() => this.webhookForm()?.nativeElement);
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
          this.notify.success('Event notification saved');
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
      .subscribe((res) => {
        this.revealedSecret.set(res.secret);
        this.notify.success('Event notification secret rotated');
      });
  }

  test(hook: WebhookResponse): void {
    this.webhookIdWithTestDeliveryInFlight.set(hook.id);
    this.service
      .test(this.scope, hook.id)
      .pipe(finalize(() => this.webhookIdWithTestDeliveryInFlight.set(null)))
      .subscribe({
        next: (delivery) => {
          this.webhooks.update((hooks) => hooks.map((row) => (row.id === hook.id ? { ...row, last_delivery: delivery } : row)));
          if (delivery.success) return this.notify.success('Test event delivered');
          const reason = getDeliveryResultReason(delivery);
          this.notify.failure("Couldn't deliver test event" + (reason ? `: ${reason}` : ''), null);
        },
        error: (err: unknown) => this.notify.failure("Couldn't send test event", err),
      });
  }

  openDeliveries(hook: WebhookResponse): void {
    this.deliveriesFor.set(hook);
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
