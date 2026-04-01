/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { DialogModule } from 'primeng/dialog';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';
import { CachesService, UpstreamCache, CacheSubscriptionMode } from '@core/services/caches.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';

@Component({
  selector: 'app-cache-upstreams',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    DialogModule,
    ButtonModule,
    InputTextModule,
    LoadingSpinnerComponent,
  ],
  templateUrl: './cache-upstreams.component.html',
  styleUrl: './cache-upstreams.component.scss',
})
export class CacheUpstreamsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private cachesService = inject(CachesService);

  loading = signal(true);
  addingUpstream = signal(false);
  savingUpstream = signal(false);
  removingUpstreamId = signal<string | null>(null);

  upstreams = signal<UpstreamCache[]>([]);
  showAddDialog = signal(false);
  showEditDialog = signal(false);
  editingUpstream = signal<UpstreamCache | null>(null);

  cacheName = '';
  cacheDisplayName = '';

  upstreamType: 'internal' | 'external' = 'internal';
  upstreamForm = {
    cache_name: '',
    display_name: '',
    url: '',
    public_key: '',
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
    this.loadUpstreams();
  }

  private loadCache(): void {
    this.cachesService.getCache(this.cacheName).subscribe({
      next: (c) => { this.cacheDisplayName = c.display_name; },
      error: () => {},
    });
  }

  loadUpstreams(): void {
    this.loading.set(true);
    this.cachesService.getCacheUpstreams(this.cacheName).subscribe({
      next: (list) => {
        this.upstreams.set(list);
        this.loading.set(false);
      },
      error: () => this.loading.set(false),
    });
  }

  openAddDialog(): void {
    this.upstreamType = 'internal';
    this.upstreamForm = { cache_name: '', display_name: '', url: '', public_key: '', mode: 'ReadWrite' };
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
    this.showEditDialog.set(true);
  }

  saveUpstream(): void {
    const upstream = this.editingUpstream();
    if (!upstream) return;
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
        this.loadUpstreams();
      },
      error: () => this.savingUpstream.set(false),
    });
  }

  addUpstream(): void {
    this.addingUpstream.set(true);
    const obs = this.upstreamType === 'internal'
      ? this.cachesService.addInternalUpstream(this.cacheName, {
          cache_name: this.upstreamForm.cache_name,
          display_name: this.upstreamForm.display_name || undefined,
          mode: this.upstreamForm.mode,
        })
      : this.cachesService.addExternalUpstream(this.cacheName, {
          display_name: this.upstreamForm.display_name,
          url: this.upstreamForm.url,
          public_key: this.upstreamForm.public_key,
        });

    obs.subscribe({
      next: () => {
        this.addingUpstream.set(false);
        this.showAddDialog.set(false);
        this.loadUpstreams();
      },
      error: (err) => {
        console.error('Failed to add upstream:', err);
        this.addingUpstream.set(false);
      },
    });
  }

  removeUpstream(id: string): void {
    this.removingUpstreamId.set(id);
    this.cachesService.removeUpstream(this.cacheName, id).subscribe({
      next: () => {
        this.removingUpstreamId.set(null);
        this.loadUpstreams();
      },
      error: () => this.removingUpstreamId.set(null),
    });
  }

  modeLabel(mode: CacheSubscriptionMode): string {
    return this.modes.find((m) => m.value === mode)?.label ?? mode;
  }
}
