/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import {
  Component,
  EventEmitter,
  Input,
  OnChanges,
  Output,
  SimpleChanges,
  inject,
  signal,
} from '@angular/core';
import { CommonModule } from '@angular/common';
import { DialogModule } from 'primeng/dialog';
import { ButtonModule } from 'primeng/button';
import { CachesService, NarDetail, NarSummary } from '@core/services/caches.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';

@Component({
  selector: 'app-cache-nars-detail-drawer',
  standalone: true,
  imports: [CommonModule, DialogModule, ButtonModule, LoadingSpinnerComponent],
  templateUrl: './cache-nars-detail-drawer.component.html',
  styleUrl: './cache-nars-detail-drawer.component.scss',
})
export class CacheNarsDetailDrawerComponent implements OnChanges {
  private cachesService = inject(CachesService);

  @Input() cacheName = '';
  @Input() summary: NarSummary | null = null;
  @Output() closed = new EventEmitter<void>();

  visible = signal(false);
  loading = signal(false);
  detail = signal<NarDetail | null>(null);
  error = signal<string | null>(null);

  ngOnChanges(changes: SimpleChanges): void {
    if ('summary' in changes) {
      if (this.summary) {
        this.visible.set(true);
        this.load(this.summary.hash);
      } else {
        this.visible.set(false);
        this.detail.set(null);
        this.error.set(null);
      }
    }
  }

  onVisibleChange(open: boolean): void {
    if (!open) {
      this.closed.emit();
    }
  }

  private load(hash: string): void {
    this.loading.set(true);
    this.error.set(null);
    this.detail.set(null);
    this.cachesService.getCacheNar(this.cacheName, hash).subscribe({
      next: (d) => {
        this.detail.set(d);
        this.loading.set(false);
      },
      error: (err) => {
        this.error.set(err?.message ?? 'Failed to load NAR details.');
        this.loading.set(false);
      },
    });
  }

  formatBytes(bytes: number | null | undefined): string {
    if (bytes === null || bytes === undefined || bytes <= 0) return '0 B';
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    const i = Math.max(0, Math.floor(Math.log(bytes) / Math.log(1024)));
    return `${(bytes / Math.pow(1024, i)).toFixed(1)} ${units[Math.min(i, units.length - 1)]}`;
  }
}
