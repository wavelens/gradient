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
  BadgeSeverity,
  ButtonComponent,
  CheckboxComponent,
  DialogComponent,
  FormFieldComponent,
  IconComponent,
  InputDirective,
  LoadStateComponent,
  MessageService,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
  SelectComponent,
} from '@gradient/ui/ui';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import { LabelHelpComponent } from '@shared/ui';
import { ConfirmService } from '@shared/confirm';
import { WritableDirective, ManagedDisableDirective, AccessService } from '@shared/access';
import { injectCacheAccess } from '@core/resolvers/inject-access';
import { normalizeProbeUrl, isGradientCacheInfo } from './cache-upstream-probe';
import { UpstreamCacheItem, CacheSubscriptionMode, PatchUpstreamRequest, ProtocolProbe } from '@core/models';
import { fieldChecks } from '@shared/forms';

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

const hasError = (errors: UpstreamErrors): boolean => Object.values(errors).some((message) => !!message);

@Component({
  selector: 'app-upstream-caches',
  standalone: true,
  imports: [
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
    ManagedDisableDirective,
    IconComponent,
    PageLayoutComponent,
    FormFieldComponent,
    SelectComponent,
    BadgeComponent,
    RowListComponent,
    RowComponent,
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
  private messageService = inject(MessageService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);

  access = injectCacheAccess();

  rowBusy = computed(() => this.togglingUpstreamId() !== null || this.testingUpstreamId() !== null);

  rowDisabled = computed(() => this.rowBusy() || this.accessSvc.shouldDisableInput(this.access()));

  loading = signal(true);
  loadError = signal<unknown>(null);
  addingUpstream = signal(false);
  savingUpstream = signal(false);
  testingUpstreamId = signal<string | null>(null);
  togglingUpstreamId = signal<string | null>(null);
  probeSuggestsProto = signal(false);

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

  upstreamType: 'internal' | 'gradient_proto' | 'http' = 'internal';
  upstreamForm = {
    cache_name: '',
    display_name: '',
    url: '',
    public_key: '',
    http1_only: false,
    remote_cache: '',
    api_key: '',
    mode: 'ReadWrite' as CacheSubscriptionMode,
  };

  editForm = {
    display_name: '',
    mode: 'ReadWrite' as CacheSubscriptionMode,
    url: '',
    public_key: '',
    http1_only: false,
  };

  readonly modes: { value: CacheSubscriptionMode; label: string }[] = [
    { value: 'ReadWrite', label: 'Read & Write' },
    { value: 'ReadOnly', label: 'Read Only' },
    { value: 'WriteOnly', label: 'Write Only' },
  ];

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

  openAddDialog(): void {
    this.upstreamType = 'internal';
    this.upstreamForm = { cache_name: '', display_name: '', url: '', public_key: '', http1_only: false, remote_cache: '', api_key: '', mode: 'ReadWrite' };
    this.addError.set(null);
    this.addAttempted.set(false);
    this.probeSuggestsProto.set(false);
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

  probeHttpUrl(): Promise<void> {
    this.probeSuggestsProto.set(false);
    const url = normalizeProbeUrl(this.upstreamForm.url);
    if (!url) return Promise.resolve();
    return fetch(`${url}/gradient-cache-info?json`, { method: 'GET', mode: 'cors' })
      .then((r) => (r.ok ? r.json() : null))
      .then((body) => { if (isGradientCacheInfo(body)) this.probeSuggestsProto.set(true); })
      .catch(() => {});
  }

  switchToGradientProto(): void {
    this.upstreamType = 'gradient_proto';
    this.probeSuggestsProto.set(false);
  }

  testUpstream(id: string): void {
    this.testingUpstreamId.set(id);
    this.cachesService.testUpstream(this.cacheName, id).subscribe({
      next: (r) => {
        this.testingUpstreamId.set(null);
        this.messageService.add({
          severity: r.ok ? 'success' : 'error',
          summary: r.message,
          detail: `HTTP/1.1: ${probeSummary(r.http1)}; HTTP/2: ${probeSummary(r.http2)}`,
        });
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
        title: 'Remove upstream cache',
        target: upstream.display_name,
        consequence: `This cache stops using ${upstream.display_name} as an upstream. You can add it again later.`,
        confirmLabel: 'Remove',
        failure: 'Failed to remove the upstream cache.',
        action: () => this.cachesService.removeUpstream(this.cacheName, upstream.id),
      })
      .subscribe(() => {
        this.notify.success('Upstream cache removed');
        this.loadUpstreamCaches();
      });
  }

  /// Writing is the consequential mode, so it reads warmer than reading.
  modeSeverity(mode: CacheSubscriptionMode): BadgeSeverity {
    switch (mode) {
      case 'ReadOnly': return 'neutral';
      case 'WriteOnly': return 'warning';
      default: return 'info';
    }
  }

  modeLabel(mode: CacheSubscriptionMode): string {
    return this.modes.find((m) => m.value === mode)?.label ?? mode;
  }
}

export function probeSummary(probe: ProtocolProbe): string {
  if (probe.ok) return `ok (${probe.latency_ms} ms)`;
  return `failed - ${probe.error ?? `status ${probe.status}`}`;
}
