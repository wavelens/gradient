/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { CachesService } from '@core/services/caches.service';
import {
  BadgeComponent,
  ButtonComponent,
  CheckboxComponent,
  DialogComponent,
  FormFieldComponent,
  InputDirective,
  LoadStateComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
  SelectComponent,
  TabSwitchComponent,
  SubmitDirective,
} from '@gradient/ui/ui';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import { LabelHelpComponent } from '@shared/ui';
import { ConfirmService } from '@shared/confirm';
import { WritableDirective, ManagedDisableDirective, ServerConfigurationManagedBannerComponent, AccessService } from '@shared/access';
import { injectCacheAccessTo } from '@core/resolvers/inject-access';
import { UpstreamCacheItem, CacheSubscriptionMode, PatchUpstreamRequest, ProtocolProbe } from '@core/models';
import { CreateCacheDialogComponent, fieldChecks, injectCanCreateCache } from '@shared/forms';
import { links } from '@shared/routing';
import { SUBSCRIPTION_MODES, subscriptionModeLabel } from '@shared/text';

type UpstreamType = 'internal' | 'gradient_proto' | 'http';
type UpstreamErrors = Partial<Record<'cache_name' | 'display_name' | 'url' | 'remote_cache' | 'public_key', string | null>>;

const DISPLAY_NAME_REQUIRED = 'Enter a display name.';
const NIX_PUBLIC_KEY = /^[^\s:]+:[A-Za-z0-9+/]+={0,2}$/;

const required = (value: string, attempted: boolean, message: string): string | null =>
  attempted && !value.trim() ? message : null;

const urlError = (value: string, attempted: boolean): string | null =>
  required(value, attempted, 'Enter the cache URL.') ?? fieldChecks.httpUrl(value);

function publicKeyError(value: string, attempted: boolean): string | null {
  const key = value.trim();
  if (!key) return attempted ? 'Enter the public key.' : null;
  return NIX_PUBLIC_KEY.test(key)
    ? null
    : 'Enter the key in name:key format. Example: cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=.';
}

interface ConnectionTestResult {
  ok: boolean;
  message: string;
  detail: string;
}

const hasError = (errors: UpstreamErrors): boolean => Object.values(errors).some((message) => !!message);

@Component({
  selector: 'app-upstream-caches',
  standalone: true,
  imports: [
    CreateCacheDialogComponent,
    LabelHelpComponent,
    CommonModule,
    RouterModule,
    FormsModule,
    DialogComponent,
    ButtonComponent,
    CheckboxComponent,
    InputDirective,
    LoadStateComponent,
    WritableDirective,
    ManagedDisableDirective, ServerConfigurationManagedBannerComponent,
    PageLayoutComponent,
    FormFieldComponent,
    SelectComponent,
    TabSwitchComponent,
    BadgeComponent,
    RowListComponent,
    RowComponent,
    SubmitDirective,
  ],
  templateUrl: './upstream-caches.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './upstream-caches.component.scss',
})
export class UpstreamCachesComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private cachesService = inject(CachesService);
  private crumbs = inject(BreadcrumbsService);
  private accessSvc = inject(AccessService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);
  showCreateCache = signal(false);
  private createForUpstream = false;
  canCreateCache = injectCanCreateCache();

  access = injectCacheAccessTo('manageUpstreamCaches');

  rowBusy = computed(() => this.togglingUpstreamId() !== null || this.testingUpstreamId() !== null);

  loading = signal(true);
  loadError = signal<unknown>(null);
  addingUpstream = signal(false);
  savingUpstream = signal(false);
  testingUpstreamId = signal<string | null>(null);
  upstreamConnectionTestResultByUpstreamId = signal<Record<string, ConnectionTestResult>>({});
  togglingUpstreamId = signal<string | null>(null);

  upstreamCaches = signal<UpstreamCacheItem[]>([]);
  showAddDialog = signal(false);
  showEditDialog = signal(false);
  editingUpstream = signal<UpstreamCacheItem | null>(null);
  addError = signal<string | null>(null);
  editError = signal<string | null>(null);
  addAttempted = signal(false);
  editAttempted = signal(false);

  cacheName = '';
  breadcrumb = computed(() => this.crumbs.cacheSettings(this.cacheName, { label: 'Upstream Caches' }));

  upstreamType: UpstreamType = 'internal';
  upstreamForm = {
    cache_name: '',
    display_name: '',
    url: '',
    public_key: '',
    http1_only: false,
    remote_cache: '',
    api_key: '',
    mode: 'ReadOnly' as CacheSubscriptionMode,
  };

  editForm = {
    display_name: '',
    mode: 'ReadWrite' as CacheSubscriptionMode,
    url: '',
    public_key: '',
    http1_only: false,
  };

  readonly modes = SUBSCRIPTION_MODES;
  readonly upstreamTypeOptions = [
    { label: 'Internal', value: 'internal' },
    { label: 'Gradient Proto', value: 'gradient_proto' },
    { label: 'HTTP', value: 'http' },
  ] as const;

  ngOnInit(): void {
    this.cacheName = this.route.snapshot.paramMap.get('cache') || '';
    this.loadUpstreamCaches();
  }

  loadUpstreamCaches(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.cachesService.getUpstreamCaches(this.cacheName).subscribe({
      next: (list) => {
        this.upstreamCaches.set(list);
        this.upstreamConnectionTestResultByUpstreamId.set({});
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  addErrors(): UpstreamErrors {
    const form = this.upstreamForm;
    const attempted = this.addAttempted();
    switch (this.upstreamType) {
      case 'internal':
        return { cache_name: required(form.cache_name, attempted, 'Enter a cache name.') };
      case 'gradient_proto':
        return {
          display_name: required(form.display_name, attempted, DISPLAY_NAME_REQUIRED),
          url: urlError(form.url, attempted),
          remote_cache: required(form.remote_cache, attempted, 'Enter the cache name on the other server.'),
        };
      case 'http':
        return {
          display_name: required(form.display_name, attempted, DISPLAY_NAME_REQUIRED),
          url: urlError(form.url, attempted),
          public_key: publicKeyError(form.public_key, attempted),
        };
    }
  }

  editErrors(): UpstreamErrors {
    const form = this.editForm;
    const attempted = this.editAttempted();
    const errors: UpstreamErrors = { display_name: required(form.display_name, attempted, DISPLAY_NAME_REQUIRED) };
    if (this.editingUpstream()?.kind !== 'http') return errors;
    return { ...errors, url: urlError(form.url, attempted), public_key: publicKeyError(form.public_key, attempted) };
  }

  kindLabel(upstream: UpstreamCacheItem): string {
    switch (upstream.kind) {
      case 'internal':
        return 'Internal';
      case 'http':
        return 'HTTP';
      default:
        return 'Gradient Proto';
    }
  }

  createCacheInsteadOfName(): void {
    this.createForUpstream = true;
    this.showAddDialog.set(false);
    this.showCreateCache.set(true);
  }

  cacheCreated(cache: string): void {
    this.createForUpstream = false;
    this.upstreamForm.cache_name = cache;
    this.showAddDialog.set(true);
  }

  createCacheClosed(visible: boolean): void {
    if (visible || !this.createForUpstream) return;
    this.createForUpstream = false;
    this.showAddDialog.set(true);
  }

  openAddDialog(): void {
    this.upstreamType = 'internal';
    this.upstreamForm = { cache_name: '', display_name: '', url: '', public_key: '', http1_only: false, remote_cache: '', api_key: '', mode: 'ReadOnly' };
    this.addError.set(null);
    this.addAttempted.set(false);
    this.showAddDialog.set(true);
  }

  openEditDialog(upstream: UpstreamCacheItem): void {
    this.editingUpstream.set(upstream);
    this.editForm = {
      display_name: upstream.display_name,
      mode: upstream.mode,
      url: upstream.url ?? '',
      public_key: upstream.public_key ?? '',
      http1_only: upstream.http1_only,
    };
    this.editError.set(null);
    this.editAttempted.set(false);
    this.showEditDialog.set(true);
  }

  saveUpstream(): void {
    const upstream = this.editingUpstream();
    if (!upstream || this.savingUpstream()) return;
    this.editAttempted.set(true);
    if (hasError(this.editErrors())) return;
    this.editError.set(null);
    this.savingUpstream.set(true);
    const data: PatchUpstreamRequest = { display_name: this.editForm.display_name.trim() };
    if (upstream.kind === 'http') {
      data.url = this.editForm.url.trim();
      data.public_key = this.editForm.public_key.trim();
      data.http1_only = this.editForm.http1_only;
    } else {
      data.mode = this.editForm.mode;
    }
    this.cachesService.updateUpstream(this.cacheName, upstream.id, data).subscribe({
      next: () => {
        this.savingUpstream.set(false);
        this.showEditDialog.set(false);
        this.notify.success('Upstream cache saved');
        this.loadUpstreamCaches();
      },
      error: (error: unknown) => {
        this.editError.set(apiErrorMessage(error, 'Upstream cache save failed.'));
        this.savingUpstream.set(false);
      },
    });
  }

  addUpstream(): void {
    if (this.addingUpstream()) return;
    this.addAttempted.set(true);
    if (hasError(this.addErrors())) return;
    this.addError.set(null);
    this.addingUpstream.set(true);
    let obs;
    switch (this.upstreamType) {
      case 'internal':
        obs = this.cachesService.addInternalUpstream(this.cacheName, {
          cache_name: this.upstreamForm.cache_name,
          display_name: this.upstreamForm.display_name || undefined,
          mode: this.upstreamForm.mode,
        });
        break;
      case 'gradient_proto':
        obs = this.cachesService.addGradientProtoUpstream(this.cacheName, {
          url: this.upstreamForm.url,
          remote_cache: this.upstreamForm.remote_cache,
          display_name: this.upstreamForm.display_name,
          mode: this.upstreamForm.mode,
          api_key: this.upstreamForm.api_key || undefined,
        });
        break;
      default:
        obs = this.cachesService.addHttpUpstream(this.cacheName, {
          display_name: this.upstreamForm.display_name,
          url: this.upstreamForm.url,
          public_key: this.upstreamForm.public_key,
          http1_only: this.upstreamForm.http1_only,
        });
    }
    obs.subscribe({
      next: () => {
        this.addingUpstream.set(false);
        this.showAddDialog.set(false);
        this.notify.success('Upstream cache added');
        this.loadUpstreamCaches();
      },
      error: (error: unknown) => {
        this.addError.set(apiErrorMessage(error, 'Failed to add upstream cache.'));
        this.addingUpstream.set(false);
      },
    });
  }

  switchUpstreamType(type: UpstreamType): void {
    this.upstreamType = type;
    this.addError.set(null);
    this.addAttempted.set(false);
  }

  testUpstream(upstream: UpstreamCacheItem): void {
    this.testingUpstreamId.set(upstream.id);
    this.cachesService.testUpstream(this.cacheName, upstream.id).subscribe({
      next: (r) => {
        this.testingUpstreamId.set(null);
        const http1OnlyHint = r.http1.ok && !r.http2.ok && !upstream.http1_only ? ' HTTP/1.1 only works. Turn on "HTTP/1.1 only" in Edit.' : '';
        this.upstreamConnectionTestResultByUpstreamId.update((results) => ({
          ...results,
          [upstream.id]: {
            ok: r.ok,
            message: r.message,
            detail: `HTTP/1.1: ${probeSummary(r.http1)}; HTTP/2: ${probeSummary(r.http2)}.${http1OnlyHint}`,
          },
        }));
      },
      error: (error: unknown) => {
        this.testingUpstreamId.set(null);
        this.notify.failure('Upstream test failed', error);
      },
    });
  }

  toggleUpstream(upstream: UpstreamCacheItem): void {
    this.togglingUpstreamId.set(upstream.id);
    this.cachesService.updateUpstream(this.cacheName, upstream.id, { active: !upstream.active }).subscribe({
      next: () => {
        this.togglingUpstreamId.set(null);
        this.notify.success(upstream.active ? 'Upstream cache deactivated' : 'Upstream cache activated');
        this.loadUpstreamCaches();
      },
      error: (error: unknown) => {
        this.togglingUpstreamId.set(null);
        this.notify.failure(
          upstream.active ? 'Upstream cache deactivation failed' : 'Upstream cache activation failed',
          error,
        );
      },
    });
  }

  removeUpstream(upstream: UpstreamCacheItem): void {
    this.confirm
      .confirm({
        title: 'Delete upstream cache',
        target: upstream.display_name,
        consequence: `This cache stops using ${upstream.display_name} as an upstream. You can add it again later.`,
        confirmLabel: 'Delete',
        failure: "Couldn't delete upstream cache",
        action: () => this.cachesService.removeUpstream(this.cacheName, upstream.id),
      })
      .subscribe(() => {
        this.notify.success('Upstream cache deleted');
        this.loadUpstreamCaches();
      });
  }

  /// Writing is the consequential mode, so it reads warmer than reading.

  protected readonly modeLabel = subscriptionModeLabel;
  protected readonly links = links;
}

export function probeSummary(probe: ProtocolProbe): string {
  if (probe.ok) return `ok (${probe.latency_ms} ms)`;
  return `failed - ${probe.error ?? `status ${probe.status}`}`;
}
