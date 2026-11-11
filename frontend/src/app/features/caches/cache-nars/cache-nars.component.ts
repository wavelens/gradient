/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { CommonModule } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { CachesService } from '@core/services/caches.service';
import { NotifyService } from '@core/services/notify.service';
import { ConfirmService } from '@shared/confirm';
import {
  BadgeComponent,
  ButtonComponent,
  CardGridComponent,
  IconComponent,
  InputDirective,
  LoadStateComponent,
  PageLayoutComponent,
  SelectButtonComponent,
  SkeletonComponent,
  StatCardComponent,
  TableComponent,
} from '@gradient/ui/ui';
import { WritableDirective } from '@shared/access';
import { injectCacheAccessTo } from '@core/resolvers/inject-access';
import { CacheNarsDetailDrawerComponent } from './cache-nars-detail-drawer.component';
import { CacheNarsPinScopeDialogComponent } from './cache-nars-pin-scope-dialog.component';
import { formatBytes, formatCount, RelativeTimePipe } from '@shared/text';
import { narSearchText, parseNarSearch } from './nar-search';
import { NarSummary, NarStats, Paginated } from '@core/models';
import { links } from '@shared/routing';

type SortKey = 'created_at' | 'nar_size' | 'last_fetched_at';
type SortOrder = 'asc' | 'desc';
type PackagePinFilter = 'all' | 'pinned' | 'unpinned';

function convertPinnedQueryValueToFilter(value: string | null): PackagePinFilter {
  if (value === 'true') return 'pinned';
  if (value === 'false') return 'unpinned';
  return 'all';
}

@Component({
  selector: 'app-cache-nars',
  standalone: true,
  imports: [
    RelativeTimePipe,
    CommonModule,
    FormsModule,
    RouterModule,
    ButtonComponent,
    InputDirective,
    SkeletonComponent,
    WritableDirective,
    CacheNarsDetailDrawerComponent,
    CacheNarsPinScopeDialogComponent,
    BadgeComponent,
    SelectButtonComponent,
    IconComponent,
    PageLayoutComponent,
    LoadStateComponent,
    StatCardComponent,
    CardGridComponent,
    TableComponent,
  ],
  templateUrl: './cache-nars.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './cache-nars.component.scss',
})
export class CacheNarsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private cachesService = inject(CachesService);
  private crumbs = inject(BreadcrumbsService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);

  protected readonly links = links;
  access = injectCacheAccessTo('writeStore');

  cacheName = '';
  breadcrumb = computed(() => this.crumbs.cache(this.cacheName, { label: 'Packages' }));

  search = signal('');
  hasSearchFilterFromUrl = signal(false);
  private filter = computed(() => parseNarSearch(this.search()));
  sort = signal<SortKey>('created_at');
  order = signal<SortOrder>('desc');
  pinnedStateFilter = signal<PackagePinFilter>('all');
  readonly pinnedStateFilterOptions: { label: string; value: PackagePinFilter }[] = [
    { label: 'All', value: 'all' },
    { label: 'Pinned', value: 'pinned' },
    { label: 'Unpinned', value: 'unpinned' },
  ];
  page = signal(1);
  perPage = signal(50);

  loading = signal(false);
  rows = signal<NarSummary[] | null>(null);
  total = signal(0);
  stats = signal<NarStats | null>(null);
  statsError = signal<unknown>(null);
  readonly placeholders = Array.from({ length: 10 });
  loadError = signal<unknown>(null);

  selected = signal<NarSummary | null>(null);
  pinDialogPackageRow = signal<NarSummary | null>(null);

  totalPages = computed(() => {
    const pp = this.perPage();
    return pp > 0 ? Math.max(1, Math.ceil(this.total() / pp)) : 1;
  });

  ngOnInit(): void {
    this.cacheName = this.route.snapshot.paramMap.get('cache') || '';
    this.route.queryParamMap.subscribe((q) => {
      const hash = q.get('hash') || undefined;
      const pkg = q.get('package') || undefined;
      this.search.set(narSearchText({ hash, package: pkg }));
      this.pinnedStateFilter.set(convertPinnedQueryValueToFilter(q.get('pinned')));
      this.hasSearchFilterFromUrl.set(!!(hash || pkg) || this.pinnedStateFilter() !== 'all');
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
      ...this.filter(),
      pinned: this.pinnedStateFilter() === 'all' ? undefined : this.pinnedStateFilter() === 'pinned',
      sort: this.sort(),
      order: this.order(),
      page: this.page(),
      per_page: this.perPage(),
    }).subscribe({
      next: (r: Paginated<NarSummary[]>) => {
        this.rows.set(r.items);
        this.total.set(r.total);
        if (!r.items.length && r.total > 0 && this.page() > 1) this.goToPage(this.totalPages());
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
    this.loadStats();
  }

  loadStats(): void {
    this.cachesService.getCacheNarStats(this.cacheName).subscribe({
      next: (s) => {
        this.stats.set(s);
        this.statsError.set(null);
      },
      error: (error: unknown) => this.statsError.set(error),
    });
  }

  applyFilters(): void {
    this.router.navigate([], {
      relativeTo: this.route,
      queryParams: {
        hash: this.filter().hash ?? null,
        package: this.filter().package ?? null,
        pinned: this.pinnedStateFilter() === 'all' ? null : this.pinnedStateFilter() === 'pinned',
        sort: this.sort(),
        order: this.order(),
        page: 1,
      },
      queryParamsHandling: 'merge',
    });
  }

  reset(): void {
    this.search.set('');
    this.pinnedStateFilter.set('all');
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

  /// `aria-sort` belongs on the header cell, and only on the sorted one.
  ariaSort(key: SortKey): 'ascending' | 'descending' | null {
    if (this.sort() !== key) return null;
    return this.order() === 'asc' ? 'ascending' : 'descending';
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

  setPinFilterAndApplyFiltersToUrl(value: PackagePinFilter): void {
    this.pinnedStateFilter.set(value);
    this.applyFilters();
  }

  openPinDialog(row: NarSummary): void {
    this.pinDialogPackageRow.set(row);
  }

  askDelete(row: NarSummary): void {
    const pinnedPackageDeleteExtraSentence = row.pinned_at ? ' Pinned package. Delete this package to clear the pin too.' : '';
    this.confirm
      .confirm({
        title: 'Delete package',
        target: `${row.package} (${row.hash})`,
        consequence:
          "Other caches can still contain this package. Then the system can remove only this cache's signature. If other caches contain no copy, the system can delete the package completely." +
          pinnedPackageDeleteExtraSentence,
        confirmLabel: 'Delete',
        failure: 'Failed to delete package.',
        action: () => this.cachesService.deleteCacheNar(this.cacheName, row.hash),
      })
      .subscribe(() => {
        this.rows.update((rows) => rows?.filter((r) => r.hash !== row.hash) ?? rows);
        this.total.set(Math.max(0, this.total() - 1));
        if (!this.rows()?.length && this.page() > 1) this.goToPage(this.page() - 1);
        this.notify.success('Package deleted');
        this.loadStats();
      });
  }

  readonly formatBytes = formatBytes;
  readonly formatCount = formatCount;
}
