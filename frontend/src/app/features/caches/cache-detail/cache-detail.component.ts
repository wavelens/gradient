/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { ButtonModule } from 'primeng/button';
import { CardModule } from 'primeng/card';
import { CachesService } from '@core/services/caches.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { Cache } from '@core/models';

@Component({
  selector: 'app-cache-detail',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    ButtonModule,
    CardModule,
    LoadingSpinnerComponent,
  ],
  templateUrl: './cache-detail.component.html',
  styleUrl: './cache-detail.component.scss',
})
export class CacheDetailComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private cachesService = inject(CachesService);

  loading = signal(true);
  cache = signal<Cache | null>(null);
  copied = signal<string | null>(null);

  cacheName = '';
  cacheUrl = '';

  ngOnInit(): void {
    this.cacheName = this.route.snapshot.paramMap.get('cache') || '';
    this.cacheUrl = `${window.location.origin}/cache/${this.cacheName}`;
    this.loadCache();
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

  copy(text: string, label: string): void {
    navigator.clipboard.writeText(text).then(() => {
      this.copied.set(label);
      setTimeout(() => this.copied.set(null), 2000);
    });
  }


}
