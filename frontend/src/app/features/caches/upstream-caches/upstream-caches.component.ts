/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { CachesService, UpstreamCache, CacheSubscriptionMode, ProtocolProbe } from '@core/services/caches.service';
import {
  BadgeComponent,
  BadgeSeverity,
  ButtonComponent,
  DialogComponent,
  EmptyStateComponent,
  FormFieldComponent,
  IconComponent,
  InputDirective,
  LoadingSpinnerComponent,
  MessageService,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
  SelectComponent,
  ToastComponent,
} from '@gradient/ui/ui';
import { LabelHelpComponent } from '@shared/ui';
import { WritableDirective, ManagedDisableDirective, AccessService } from '@shared/access';
import { injectCacheAccess } from '@core/resolvers/inject-access';
import { normalizeProbeUrl, isGradientCacheInfo } from './cache-upstream-probe';

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
    InputDirective,
    LoadingSpinnerComponent,
    WritableDirective,
    ManagedDisableDirective,
    IconComponent,
    PageLayoutComponent,
    FormFieldComponent,
    EmptyStateComponent,
    SelectComponent,
    BadgeComponent,
    RowListComponent,
    RowComponent,
    ToastComponent,
  ],
  providers: [MessageService],
  templateUrl: './upstream-caches.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './upstream-caches.component.scss',
})
export class UpstreamCachesComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private cachesService = inject(CachesService);
  private accessSvc = inject(AccessService);
  private messageService = inject(MessageService);

  access = injectCacheAccess();

  rowBusy = computed(
    () =>
      this.removingUpstreamId() !== null ||
      this.togglingUpstreamId() !== null ||
      this.testingUpstreamId() !== null,
  );

  rowDisabled = computed(() => this.rowBusy() || this.accessSvc.shouldDisableInput(this.access()));

  loading = signal(true);
  addingUpstream = signal(false);
  savingUpstream = signal(false);
  removingUpstreamId = signal<string | null>(null);
  testingUpstreamId = signal<string | null>(null);
  togglingUpstreamId = signal<string | null>(null);
  probeSuggestsProto = signal(false);

  upstreamCaches = signal<UpstreamCache[]>([]);
  showAddDialog = signal(false);
  showEditDialog = signal(false);
  editingUpstream = signal<UpstreamCache | null>(null);
  addError = signal<string | null>(null);
  editError = signal<string | null>(null);

  cacheName = '';
  cacheDisplayName = '';

  upstreamType: 'internal' | 'gradient_proto' | 'http' = 'internal';
  upstreamForm = {
    cache_name: '',
    display_name: '',
    url: '',
    public_key: '',
    remote_cache: '',
    api_key: '',
    mode: 'ReadWrite' as CacheSubscriptionMode,
  };

  editForm = {
    display_name: '',
    mode: 'ReadWrite' as CacheSubscriptionMode,
    url: '',
    public_key: '',
  };

  readonly modes: { value: CacheSubscriptionMode; label: string }[] = [
    { value: 'ReadWrite', label: 'Read & Write' },
    { value: 'ReadOnly', label: 'Read Only' },
    { value: 'WriteOnly', label: 'Write Only' },
  ];

  ngOnInit(): void {
    this.cacheName = this.route.snapshot.paramMap.get('cache') || '';
    this.loadCache();
    this.loadUpstreamCaches();
  }

  private loadCache(): void {
    this.cachesService.getCache(this.cacheName).subscribe({
      next: (c) => { this.cacheDisplayName = c.display_name; },
      error: () => {},
    });
  }

  loadUpstreamCaches(): void {
    this.loading.set(true);
    this.cachesService.getUpstreamCaches(this.cacheName).subscribe({
      next: (list) => {
        this.upstreamCaches.set(list);
        this.loading.set(false);
      },
      error: () => this.loading.set(false),
    });
  }

  isAddFormValid(): boolean {
    if (this.upstreamType === 'internal') {
      return this.upstreamForm.cache_name.trim().length > 0;
    }
    if (this.upstreamType === 'gradient_proto') {
      return (
        this.upstreamForm.url.trim().length > 0 &&
        this.upstreamForm.remote_cache.trim().length > 0 &&
        this.upstreamForm.display_name.trim().length > 0
      );
    }
    return (
      this.upstreamForm.display_name.trim().length > 0 &&
      this.upstreamForm.url.trim().length > 0 &&
      this.upstreamForm.public_key.trim().length > 0
    );
  }

  isEditFormValid(): boolean {
    const upstream = this.editingUpstream();
    if (!upstream) return false;
    const isExternal = !upstream.upstream_cache_id;
    if (!isExternal) return true;
    return (
      this.editForm.display_name.trim().length > 0 &&
      this.editForm.url.trim().length > 0 &&
      this.editForm.public_key.trim().length > 0
    );
  }

  openAddDialog(): void {
    this.upstreamType = 'internal';
    this.upstreamForm = { cache_name: '', display_name: '', url: '', public_key: '', remote_cache: '', api_key: '', mode: 'ReadWrite' };
    this.addError.set(null);
    this.probeSuggestsProto.set(false);
    this.showAddDialog.set(true);
  }

  openEditDialog(upstream: UpstreamCache): void {
    this.editingUpstream.set(upstream);
    this.editForm = {
      display_name: upstream.display_name,
      mode: upstream.mode,
      url: upstream.url ?? '',
      public_key: upstream.public_key ?? '',
    };
    this.editError.set(null);
    this.showEditDialog.set(true);
  }

  saveUpstream(): void {
    const upstream = this.editingUpstream();
    if (!upstream) return;
    if (!this.isEditFormValid()) {
      this.editError.set('Please fill in all required fields.');
      return;
    }
    this.editError.set(null);
    this.savingUpstream.set(true);
    const isExternal = !upstream.upstream_cache_id;
    const data: { display_name?: string; mode?: CacheSubscriptionMode; url?: string; public_key?: string } = {
      display_name: this.editForm.display_name || undefined,
    };
    if (!isExternal) {
      data.mode = this.editForm.mode;
    } else {
      data.url = this.editForm.url || undefined;
      data.public_key = this.editForm.public_key || undefined;
    }
    this.cachesService.updateUpstream(this.cacheName, upstream.id, data).subscribe({
      next: () => {
        this.savingUpstream.set(false);
        this.showEditDialog.set(false);
        this.loadUpstreamCaches();
      },
      error: () => this.savingUpstream.set(false),
    });
  }

  addUpstream(): void {
    if (!this.isAddFormValid()) {
      this.addError.set('Please fill in all required fields.');
      return;
    }
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
        });
    }
    obs.subscribe({
      next: () => {
        this.addingUpstream.set(false);
        this.showAddDialog.set(false);
        this.loadUpstreamCaches();
      },
      error: (err) => {
        this.addError.set(err?.error?.message || err?.message || 'Failed to add upstream cache.');
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
      error: (err) => {
        this.testingUpstreamId.set(null);
        this.messageService.add({
          severity: 'error',
          summary: 'Upstream test failed',
          detail: err?.message || 'Failed to test upstream.',
        });
      },
    });
  }

  toggleUpstream(upstream: UpstreamCache): void {
    this.togglingUpstreamId.set(upstream.id);
    this.cachesService.updateUpstream(this.cacheName, upstream.id, { active: !upstream.active }).subscribe({
      next: () => {
        this.togglingUpstreamId.set(null);
        this.loadUpstreamCaches();
      },
      error: () => this.togglingUpstreamId.set(null),
    });
  }

  removeUpstream(id: string): void {
    this.removingUpstreamId.set(id);
    this.cachesService.removeUpstream(this.cacheName, id).subscribe({
      next: () => {
        this.removingUpstreamId.set(null);
        this.loadUpstreamCaches();
      },
      error: () => this.removingUpstreamId.set(null),
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
