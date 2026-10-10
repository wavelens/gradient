/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed, linkedSignal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { CachesService, CacheStats, CacheMetricPoint, StorageMetricPoint } from '@core/services/caches.service';
import { AuthService } from '@core/services/auth.service';
import { StarsService } from '@core/services/stars.service';
import { injectCacheAccessData } from '@core/resolvers/inject-access';
import {
  BadgeComponent,
  ButtonComponent,
  CardGridComponent,
  CopyFieldComponent,
  DividerComponent,
  FormFieldComponent,
  PageLayoutComponent,
  TabSwitchComponent,
  SettingsSectionComponent,
  StatCardComponent,
} from '@gradient/ui/ui';
import {
  LabelHelpComponent,
  MetricChartComponent,
  MetricSeries,
  StarButtonComponent,
} from '@shared/ui';
import { Cache, StarTarget } from '@core/models';
import { docsUrl } from '@core/docs';
import { formatBytes, formatCount } from '@shared/text';
import { FormsModule } from '@angular/forms';

type Window = 'minutes' | 'hours' | 'days' | 'weeks';

const CHART_COLORS = {
  bytes: '#17a2b8',
  requests: '#28a745',
  storageBytes: '#fd7e14',
  storagePackages: '#6f42c1',
};

@Component({
  selector: 'app-cache-detail',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    ButtonComponent,
    LabelHelpComponent,
    MetricChartComponent,
    PageLayoutComponent,
    BadgeComponent,
    CopyFieldComponent,
    SettingsSectionComponent,
    DividerComponent,
    FormFieldComponent,
    StatCardComponent,
    CardGridComponent,
    TabSwitchComponent,
    StarButtonComponent,
    FormsModule,
  ],
  templateUrl: './cache-detail.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './cache-detail.component.scss',
})
export class CacheDetailComponent implements OnInit {
  protected readonly netrcDocsUrl = docsUrl('guides/share-a-cache/#1-use-the-cache-on-a-machine');
  private route = inject(ActivatedRoute);
  private cachesService = inject(CachesService);
  private crumbs = inject(BreadcrumbsService);
  private stars = inject(StarsService);
  protected authService = inject(AuthService);

  private access = injectCacheAccessData();
  cache = linkedSignal<Cache | null>(() => this.access()?.cache ?? null);
  statsLoading = signal(true);
  stats = signal<CacheStats | null>(null);
  activeWindow = signal<Window>('hours');
  starred = signal(false);

  setWindow(value: unknown): void {
    this.activeWindow.set(value as Window);
  }

  // Only this cache's own key: the server re-signs everything it serves, paths
  // proxied from an upstream cache included, so a client never needs the key of a
  // cache we happen to proxy.
  trustedPublicKeys = computed(() => {
    const own = this.cache()?.public_key;
    return own ? [own] : [];
  });

  cacheName = '';
  breadcrumb = computed(() => this.crumbs.cache(this.cacheName));
  starTarget: StarTarget = { kind: 'cache', cache: '' };
  cacheUrl = '';
  serverUrl = '';

  nixConfSnippet = computed(() => {
    const keys = this.trustedPublicKeys();
    return `substituters = ${this.cacheUrl}\ntrusted-public-keys = ${keys.length ? keys.join(' ') : '<unavailable>'}`;
  });

  get installNetrcCommand(): string {
    return `nix run github:wavelens/gradient/latest#gradient-cli -- cache install-netrc --server ${this.serverUrl} --token <YOUR_TOKEN> --cache ${this.cacheName}`;
  }

  readonly windows: { key: Window; label: string }[] = [
    { key: 'minutes', label: 'Minutes' },
    { key: 'hours', label: 'Hours' },
    { key: 'days', label: 'Days' },
    { key: 'weeks', label: 'Weeks' },
  ];

  activePoints = computed<CacheMetricPoint[]>(() => {
    const s = this.stats();
    if (!s) return [];
    return s[this.activeWindow()];
  });

  activeStoragePoints = computed<StorageMetricPoint[]>(() => {
    const s = this.stats();
    if (!s) return [];
    const key = `storage_${this.activeWindow()}` as keyof CacheStats;
    return s[key] as StorageMetricPoint[];
  });

  trafficCategories = computed(() =>
    this.activePoints().map((p) => this.formatTime(p.time, this.activeWindow()))
  );
  trafficSeries = computed<MetricSeries[]>(() => [
    { name: 'Bytes served', data: this.activePoints().map((p) => p.bytes) },
    { name: 'Requests', data: this.activePoints().map((p) => p.requests), axis: 'right' },
  ]);

  storageCategories = computed(() =>
    this.activeStoragePoints().map((p) => this.formatTime(p.time, this.activeWindow()))
  );
  storageSeries = computed<MetricSeries[]>(() => [
    { name: 'Bytes added', data: this.activeStoragePoints().map((p) => p.bytes) },
    { name: 'Packages added', data: this.activeStoragePoints().map((p) => p.packages), axis: 'right' },
  ]);

  readonly trafficColors = [CHART_COLORS.bytes, CHART_COLORS.requests];
  readonly storageColors = [CHART_COLORS.storageBytes, CHART_COLORS.storagePackages];

  readonly formatBytes = formatBytes;
  readonly formatCount = formatCount;
  readonly trafficSecondary = { title: 'Requests', valueFormatter: (v: number) => `${formatCount(v)} req` };
  readonly storageSecondary = { title: 'Packages', valueFormatter: (v: number) => `${formatCount(v)} pkg` };

  ngOnInit(): void {
    this.cacheName = this.route.snapshot.paramMap.get('cache') || '';
    this.starTarget = { kind: 'cache', cache: this.cacheName };
    this.stars.starred(this.starTarget).subscribe((starred) => this.starred.set(starred));
    this.serverUrl = window.location.origin;
    this.cacheUrl = `${this.serverUrl}/cache/${this.cacheName}`;
    this.loadCache();
    this.loadStats();
  }

  loadCache(): void {
    this.cachesService.getCache(this.cacheName).subscribe((cache) => {
      this.cache.set(cache);
      this.crumbs.rememberCache(this.cacheName, cache.display_name);
    });
  }

  loadStats(): void {
    this.statsLoading.set(true);
    this.cachesService.getCacheStats(this.cacheName).subscribe({
      next: (stats) => {
        this.stats.set(stats);
        this.statsLoading.set(false);
      },
      error: () => this.statsLoading.set(false),
    });
  }

  private formatTime(iso: string, window: Window): string {
    const d = new Date(iso.includes('T') ? iso : iso.replace(' ', 'T') + 'Z');
    if (window === 'minutes') return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
    if (window === 'hours') return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
    if (window === 'days') return d.toLocaleDateString([], { month: 'short', day: 'numeric' });
    return d.toLocaleDateString([], { month: 'short', day: 'numeric' });
  }
}
