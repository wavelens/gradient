/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal } from '@angular/core';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { CommonModule } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { DialogModule } from 'primeng/dialog';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';
import {
  CachesService,
  NarListResponse,
  NarStats,
  NarSummary,
} from '@core/services/caches.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { WritableDirective } from '@shared/access';
import { injectCacheAccess } from '@core/resolvers/inject-access';
import { CacheNarsDetailDrawerComponent } from './cache-nars-detail-drawer.component';

type SortKey = 'created_at' | 'nar_size' | 'last_fetched_at';
type SortOrder = 'asc' | 'desc';

@Component({
  selector: 'app-cache-nars',
  standalone: true,
  imports: [
    CommonModule,
    FormsModule,
    RouterModule,
    DialogModule,
    ButtonModule,
    InputTextModule,
    LoadingSpinnerComponent,
    WritableDirective,
    CacheNarsDetailDrawerComponent,
  ],
  templateUrl: './cache-nars.component.html',
  styleUrl: './cache-nars.component.scss',
})
export class CacheNarsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private cachesService = inject(CachesService);

  access = injectCacheAccess();

  rowDisabled = computed(() => this.deletingHash() !== null);

  cacheName = '';
  cacheDisplayName = '';

  hash = signal('');
  package = signal('');
  sort = signal<SortKey>('created_at');
  order = signal<SortOrder>('desc');
  page = signal(1);
  perPage = signal(50);

  loading = signal(false);
  rows = signal<NarSummary[]>([]);
  total = signal(0);
  stats = signal<NarStats | null>(null);
  loadError = signal<string | null>(null);
  deleteError = signal<string | null>(null);

  selected = signal<NarSummary | null>(null);
  pendingDelete = signal<NarSummary | null>(null);
  deletingHash = signal<string | null>(null);

  totalPages = computed(() => {
    const pp = this.perPage();
    return pp > 0 ? Math.max(1, Math.ceil(this.total() / pp)) : 1;
  });

  ngOnInit(): void {
    this.cacheName = this.route.snapshot.paramMap.get('cache') || '';
    this.cachesService.getCache(this.cacheName).subscribe({
      next: (c) => { this.cacheDisplayName = c.display_name; },
      error: () => {},
    });
    this.route.queryParamMap.subscribe((q) => {
      this.hash.set(q.get('hash') ?? '');
      this.package.set(q.get('package') ?? '');
      this.sort.set((q.get('sort') as SortKey) ?? 'created_at');
      this.order.set((q.get('order') as SortOrder) ?? 'desc');
      const p = Number(q.get('page') ?? 1);
      this.page.set(Number.isFinite(p) && p > 0 ? p : 1);
      this.refresh();
    });
  }

  refresh(): void {
    if (!this.cacheName) return;
    this.loading.set(true);
    this.loadError.set(null);
    this.cachesService.getCacheNars(this.cacheName, {
      hash: this.hash() || undefined,
      package: this.package() || undefined,
      sort: this.sort(),
      order: this.order(),
      page: this.page(),
      per_page: this.perPage(),
    }).subscribe({
      next: (r: NarListResponse) => {
        this.rows.set(r.items);
        this.total.set(r.total);
        this.loading.set(false);
      },
      error: (err) => {
        this.loadError.set(err?.message ?? 'Failed to load NARs.');
        this.loading.set(false);
      },
    });
    this.loadStats();
  }

  private loadStats(): void {
    this.cachesService.getCacheNarStats(this.cacheName).subscribe({
      next: (s) => this.stats.set(s),
      error: () => {},
    });
  }

  applyFilters(): void {
    this.router.navigate([], {
      relativeTo: this.route,
      queryParams: {
        hash: this.hash() || null,
        package: this.package() || null,
        sort: this.sort(),
        order: this.order(),
        page: 1,
      },
      queryParamsHandling: 'merge',
    });
  }

  reset(): void {
    this.hash.set('');
    this.package.set('');
    this.sort.set('created_at');
    this.order.set('desc');
    this.router.navigate([], { relativeTo: this.route, queryParams: {} });
  }

  goToPage(page: number): void {
    const target = Math.max(1, Math.min(this.totalPages(), page));
    if (target === this.page()) return;
    this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { page: target },
      queryParamsHandling: 'merge',
    });
  }

  sortBy(key: SortKey): void {
    if (this.sort() === key) {
      this.order.set(this.order() === 'asc' ? 'desc' : 'asc');
    } else {
      this.sort.set(key);
      this.order.set('desc');
    }
    this.applyFilters();
  }

  show(row: NarSummary): void {
    this.selected.set(row);
  }

  closeDrawer(): void {
    this.selected.set(null);
  }

  askDelete(row: NarSummary): void {
    this.deleteError.set(null);
    this.pendingDelete.set(row);
  }

  cancelDelete(): void {
    if (this.deletingHash()) return;
    this.pendingDelete.set(null);
  }

  confirmDelete(): void {
    const row = this.pendingDelete();
    if (!row) return;
    this.deleteError.set(null);
    this.deletingHash.set(row.hash);
    this.cachesService.deleteCacheNar(this.cacheName, row.hash).subscribe({
      next: () => {
        this.rows.set(this.rows().filter((r) => r.hash !== row.hash));
        this.total.set(Math.max(0, this.total() - 1));
        this.deletingHash.set(null);
        this.pendingDelete.set(null);
        this.loadStats();
      },
      error: (err) => {
        this.deleteError.set(err?.message ?? 'Failed to delete NAR.');
        this.deletingHash.set(null);
      },
    });
  }

  formatDate(iso: string | null | undefined, fallback = '-'): string {
    if (!iso) return fallback;
    const d = new Date(iso.includes('T') ? iso : iso.replace(' ', 'T') + 'Z');
    return isNaN(d.getTime()) ? iso : d.toLocaleString();
  }

  formatBytes(bytes: number | null | undefined): string {
    if (bytes === null || bytes === undefined || bytes <= 0) return '0 B';
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    const i = Math.max(0, Math.floor(Math.log(bytes) / Math.log(1024)));
    return `${(bytes / Math.pow(1024, i)).toFixed(1)} ${units[Math.min(i, units.length - 1)]}`;
  }
}
