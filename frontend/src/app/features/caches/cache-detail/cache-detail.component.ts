/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { CachesService } from '@core/services/caches.service';
import { AuthService } from '@core/services/auth.service';
import { StarsService } from '@core/services/stars.service';
import { ConfigService } from '@core/services/config.service';
import { injectCacheAccess, injectCacheAccessData } from '@core/resolvers/inject-access';
import {
  BadgeComponent,
  ButtonComponent,
  CardGridComponent,
  CopyFieldComponent,
  DividerComponent,
  FormFieldComponent,
  LoadStateComponent,
  MessageBannerComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
  TabSwitchComponent,
  SettingsSectionComponent,
  StatCardComponent,
} from '@gradient/ui/ui';
import {
  LabelHelpComponent,
  MetricChartComponent,
  MetricSeries,
  StarButtonComponent,
  entityKindToIconName,
} from '@shared/ui';
import {
  StarTarget,
  CacheStatsResponse,
  CacheMetricPoint,
  StorageMetricPoint,
  CacheResponse,
  CacheSubscribersResponse,
  TrafficKind,
} from '@core/models';
import { docsUrl } from '@core/docs';
import { clockTime, formatBytes, formatCount, monthDay, plural, subscriptionModeLabel } from '@shared/text';
import { UseCacheDialogComponent } from './use-cache-dialog.component';
import { injectPendingSubscriptionRequestCount } from '../pending-subscription-request-count';
import { oneOf, queryParamSignal } from '@shared/routing';
import { FormsModule } from '@angular/forms';
import { links } from '@shared/routing';

type Window = 'minutes' | 'hours' | 'days' | 'weeks';

const CHART_COLORS = {
  bytes: '--gr-chart-cyan',
  requests: '--gr-chart-green',
  storageBytes: '--gr-chart-orange',
  storagePackages: '--gr-chart-purple',
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
    MessageBannerComponent,
    CopyFieldComponent,
    SettingsSectionComponent,
    DividerComponent,
    FormFieldComponent,
    LoadStateComponent,
    StatCardComponent,
    CardGridComponent,
    TabSwitchComponent,
    StarButtonComponent,
    FormsModule,
    RowListComponent,
    RowComponent,
    UseCacheDialogComponent,
  ],
  templateUrl: './cache-detail.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './cache-detail.component.scss',
})
export class CacheDetailComponent implements OnInit {
  protected readonly entityKindToIconName = entityKindToIconName;
  protected readonly links = links;
  protected readonly netrcDocsUrl = docsUrl('guides/share-a-cache/#1-use-the-cache-on-a-machine');
  private route = inject(ActivatedRoute);
  private cachesService = inject(CachesService);
  private crumbs = inject(BreadcrumbsService);
  private stars = inject(StarsService);
  private config = inject(ConfigService);
  protected authService = inject(AuthService);

  private access = injectCacheAccessData();
  protected editAccess = injectCacheAccess();
  cache = computed<CacheResponse | null>(() => this.access()?.cache ?? null);
  statsLoading = signal(true);
  stats = signal<CacheStatsResponse | null>(null);
  statsError = signal<unknown>(null);
  readonly windows: { key: Window; label: string }[] = [
    { key: 'minutes', label: 'Minutes' },
    { key: 'hours', label: 'Hours' },
    { key: 'days', label: 'Days' },
    { key: 'weeks', label: 'Weeks' },
  ];
  readonly trafficOptions: { key: TrafficKind; label: string }[] = [
    { key: 'total', label: 'Total' },
    { key: 'external', label: 'External' },
  ];
  activeWindow = queryParamSignal<Window>('window', 'hours', oneOf(this.windows.map((w) => w.key)));
  traffic = queryParamSignal<TrafficKind>('traffic', 'total', oneOf(this.trafficOptions.map((o) => o.key)));
  starred = signal(false);
  subscribers = signal<CacheSubscribersResponse | null>(null);
  subscribersError = signal<unknown>(null);
  useDialogOpen = signal(false);
  protected readonly modeLabel = subscriptionModeLabel;
  protected readonly plural = plural;
  pendingSubscriptionRequestCount = injectPendingSubscriptionRequestCount();

  setWindow(value: unknown): void {
    this.activeWindow.set(value as Window);
  }

  setTraffic(value: unknown): void {
    this.traffic.set(value as TrafficKind);
    this.loadStats();
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
    return `extra-substituters = ${this.cacheUrl}\nextra-trusted-public-keys = ${keys.length ? keys.join(' ') : '<unavailable>'}`;
  });

  get installNetrcCommand(): string {
    return `nix run nixpkgs#gradient-cli -- cache install-netrc --server ${this.serverUrl} --token <YOUR_TOKEN> --cache ${this.cacheName}`;
  }

  activePoints = computed<CacheMetricPoint[]>(() => {
    const s = this.stats();
    if (!s) return [];
    return s[this.activeWindow()];
  });

  activeStoragePoints = computed<StorageMetricPoint[]>(() => {
    const s = this.stats();
    if (!s) return [];
    const key = `storage_${this.activeWindow()}` as keyof CacheStatsResponse;
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
    this.serverUrl = this.config.serveUrl;
    this.cacheUrl = `${this.serverUrl}/cache/${this.cacheName}`;
    this.loadStats();
    this.loadSubscribers();
  }

  loadSubscribers(): void {
    this.subscribersError.set(null);
    this.cachesService.getSubscribers(this.cacheName).subscribe({
      next: (used) => this.subscribers.set(used),
      error: (error: unknown) => this.subscribersError.set(error),
    });
  }

  loadStats(): void {
    this.statsLoading.set(true);
    this.statsError.set(null);
    this.cachesService.getCacheStats(this.cacheName, this.traffic()).subscribe({
      next: (stats) => {
        this.stats.set(stats);
        this.statsLoading.set(false);
      },
      error: (error: unknown) => {
        this.statsError.set(error);
        this.statsLoading.set(false);
      },
    });
  }

  private formatTime(iso: string, window: Window): string {
    return window === 'minutes' || window === 'hours' ? clockTime(iso) : monthDay(iso);
  }
}
