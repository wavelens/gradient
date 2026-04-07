/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { ButtonModule } from 'primeng/button';
import { CardModule } from 'primeng/card';
import { CachesService, CacheStats, CacheMetricPoint, StorageMetricPoint, UpstreamCache } from '@core/services/caches.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { Cache } from '@core/models';
import {
  NgApexchartsModule,
  ApexChart,
  ApexStroke,
  ApexFill,
  ApexXAxis,
  ApexYAxis,
  ApexTooltip,
  ApexGrid,
  ApexDataLabels,
  ApexLegend,
} from 'ng-apexcharts';

type Window = 'minutes' | 'hours' | 'days' | 'weeks';

const CHART_COLORS = {
  bytes: '#17a2b8',
  requests: '#28a745',
  storageBytes: '#fd7e14',
  storagePackages: '#6f42c1',
  background: '#21262d',
  border: '#2d333b',
  text: '#abb0b4',
  grid: '#2d333b',
};

@Component({
  selector: 'app-cache-detail',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    ButtonModule,
    CardModule,
    LoadingSpinnerComponent,
    NgApexchartsModule,
  ],
  templateUrl: './cache-detail.component.html',
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
    return `nix run wavelens/gradient#gradient-cli -- cache install-netrc --server ${this.serverUrl} --token <YOUR_TOKEN> --cache ${this.cacheName}`;
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

  trafficChartOptions = computed<{
    chart: ApexChart;
    theme: { mode: 'dark' | 'light' };
    stroke: ApexStroke;
    fill: ApexFill;
    colors: string[];
    series: { name: string; data: number[] }[];
    xaxis: ApexXAxis;
    yaxis: ApexYAxis | ApexYAxis[];
    grid: ApexGrid;
    tooltip: ApexTooltip;
    legend: ApexLegend;
    dataLabels: ApexDataLabels;
  }>(() => {
    const points = this.activePoints();
    const categories = points.map((p) => this.formatTime(p.time, this.activeWindow()));
    return {
      chart: {
        type: 'area',
        height: 220,
        background: CHART_COLORS.background,
        toolbar: { show: false },
        sparkline: { enabled: false },
        animations: { enabled: true, speed: 400 },
      },
      theme: { mode: 'dark' },
      stroke: { curve: 'smooth', width: [2, 2] },
      fill: {
        type: 'gradient',
        gradient: {
          shadeIntensity: 0.4,
          opacityFrom: 0.5,
          opacityTo: 0.05,
          stops: [0, 100],
        },
      },
      colors: [CHART_COLORS.bytes, CHART_COLORS.requests],
      series: [
        {
          name: 'Bytes served',
          data: points.map((p) => p.bytes),
        },
        {
          name: 'Requests',
          data: points.map((p) => p.requests),
        },
      ],
      xaxis: {
        categories,
        labels: { style: { colors: CHART_COLORS.text, fontSize: '11px' }, rotate: -30 },
        axisBorder: { color: CHART_COLORS.border },
        axisTicks: { color: CHART_COLORS.border },
      },
      yaxis: [
        {
          title: { text: 'Bytes', style: { color: CHART_COLORS.text } },
          labels: {
            style: { colors: CHART_COLORS.text },
            formatter: (v: number) => this.formatBytes(v),
          },
        },
        {
          opposite: true,
          title: { text: 'Requests', style: { color: CHART_COLORS.text } },
          labels: { style: { colors: CHART_COLORS.text } },
        },
      ],
      grid: { borderColor: CHART_COLORS.grid, strokeDashArray: 3 },
      tooltip: {
        theme: 'dark',
        y: [
          { formatter: (v: number) => this.formatBytes(v) },
          { formatter: (v: number) => `${v} req` },
        ],
      },
      legend: { labels: { colors: CHART_COLORS.text } },
      dataLabels: { enabled: false },
    };
  });

  storageChartOptions = computed<{
    chart: ApexChart;
    theme: { mode: 'dark' | 'light' };
    stroke: ApexStroke;
    fill: ApexFill;
    colors: string[];
    series: { name: string; data: number[] }[];
    xaxis: ApexXAxis;
    yaxis: ApexYAxis | ApexYAxis[];
    grid: ApexGrid;
    tooltip: ApexTooltip;
    legend: ApexLegend;
    dataLabels: ApexDataLabels;
  }>(() => {
    const points = this.activeStoragePoints();
    const categories = points.map((p) => this.formatTime(p.time, this.activeWindow()));
    return {
      chart: {
        type: 'area',
        height: 220,
        background: CHART_COLORS.background,
        toolbar: { show: false },
        sparkline: { enabled: false },
        animations: { enabled: true, speed: 400 },
      },
      theme: { mode: 'dark' },
      stroke: { curve: 'smooth', width: [2, 2] },
      fill: {
        type: 'gradient',
        gradient: {
          shadeIntensity: 0.4,
          opacityFrom: 0.5,
          opacityTo: 0.05,
          stops: [0, 100],
        },
      },
      colors: [CHART_COLORS.storageBytes, CHART_COLORS.storagePackages],
      series: [
        {
          name: 'Bytes added',
          data: points.map((p) => p.bytes),
        },
        {
          name: 'Packages added',
          data: points.map((p) => p.packages),
        },
      ],
      xaxis: {
        categories,
        labels: { style: { colors: CHART_COLORS.text, fontSize: '11px' }, rotate: -30 },
        axisBorder: { color: CHART_COLORS.border },
        axisTicks: { color: CHART_COLORS.border },
      },
      yaxis: [
        {
          title: { text: 'Bytes', style: { color: CHART_COLORS.text } },
          labels: {
            style: { colors: CHART_COLORS.text },
            formatter: (v: number) => this.formatBytes(v),
          },
        },
        {
          opposite: true,
          title: { text: 'Packages', style: { color: CHART_COLORS.text } },
          labels: { style: { colors: CHART_COLORS.text } },
        },
      ],
      grid: { borderColor: CHART_COLORS.grid, strokeDashArray: 3 },
      tooltip: {
        theme: 'dark',
        y: [
          { formatter: (v: number) => this.formatBytes(v) },
          { formatter: (v: number) => `${v} pkg` },
        ],
      },
      legend: { labels: { colors: CHART_COLORS.text } },
      dataLabels: { enabled: false },
    };
  });

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
