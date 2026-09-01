/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { CachesService, CacheStats, CacheMetricPoint, StorageMetricPoint, UpstreamCache } from '@core/services/caches.service';
import {
  BadgeComponent,
  ButtonComponent,
  IconComponent,
  LabelHelpComponent,
  LoadingSpinnerComponent,
  MetricChartComponent,
  MetricSeries,
  PageLayoutComponent,
} from '@shared/ui';
import { Cache } from '@core/models';

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
    LoadingSpinnerComponent,
    LabelHelpComponent,
    MetricChartComponent,
    IconComponent,
    PageLayoutComponent,
    BadgeComponent,
  ],
  templateUrl: './cache-detail.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './cache-detail.component.scss',
})
export class CacheDetailComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private cachesService = inject(CachesService);

  loading = signal(true);
  statsLoading = signal(true);
  cache = signal<Cache | null>(null);
  upstreams = signal<UpstreamCache[]>([]);
  stats = signal<CacheStats | null>(null);
  copied = signal<string | null>(null);
  activeWindow = signal<Window>('hours');

  externalUpstreamKeys = computed(() =>
    this.upstreams()
      .filter(u => u.public_key)
      .map(u => u.public_key!)
  );

  allPublicKeys = computed(() => {
    const own = this.cache()?.public_key;
    return [...(own ? [own] : []), ...this.externalUpstreamKeys()];
  });

  cacheName = '';
  cacheUrl = '';
  serverUrl = '';

  get installNetrcCommand(): string {
    return `nix run github:wavelens/gradient#gradient-cli -- cache install-netrc --server ${this.serverUrl} --token <YOUR_TOKEN> --cache ${this.cacheName}`;
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

  readonly formatSize = (v: number) => this.formatBytes(v);
  readonly trafficSecondary = { title: 'Requests', valueFormatter: (v: number) => `${v} req` };
  readonly storageSecondary = { title: 'Packages', valueFormatter: (v: number) => `${v} pkg` };

  ngOnInit(): void {
    this.cacheName = this.route.snapshot.paramMap.get('cache') || '';
    this.serverUrl = window.location.origin;
    this.cacheUrl = `${this.serverUrl}/cache/${this.cacheName}`;
    this.loadCache();
    this.loadStats();
    this.loadUpstreams();
  }

  loadCache(): void {
    this.loading.set(true);
    this.cachesService.getCache(this.cacheName).subscribe({
      next: (cache) => {
        this.cache.set(cache);
        this.loading.set(false);
      },
      error: (error) => {
        console.error('Failed to load cache:', error);
        this.loading.set(false);
      },
    });
  }

  loadUpstreams(): void {
    this.cachesService.getCacheUpstreams(this.cacheName).subscribe({
      next: (upstreams) => this.upstreams.set(upstreams),
      error: () => {},
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

  copy(text: string, label: string): void {
    navigator.clipboard.writeText(text).then(() => {
      this.copied.set(label);
      setTimeout(() => this.copied.set(null), 2000);
    });
  }

  formatBytes(bytes: number): string {
    if (bytes <= 0) return '0 B';
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    const i = Math.max(0, Math.floor(Math.log(bytes) / Math.log(1024)));
    return `${(bytes / Math.pow(1024, i)).toFixed(1)} ${units[Math.min(i, units.length - 1)]}`;
  }

  private formatTime(iso: string, window: Window): string {
    const d = new Date(iso.includes('T') ? iso : iso.replace(' ', 'T') + 'Z');
    if (window === 'minutes') return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
    if (window === 'hours') return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
    if (window === 'days') return d.toLocaleDateString([], { month: 'short', day: 'numeric' });
    return d.toLocaleDateString([], { month: 'short', day: 'numeric' });
  }
}
