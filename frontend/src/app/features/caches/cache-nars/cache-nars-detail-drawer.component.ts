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
  ChangeDetectionStrategy
} from '@angular/core';
import { CommonModule } from '@angular/common';
import { CachesService, NarDetail, NarSummary } from '@core/services/caches.service';
import {
  BadgeComponent,
  ButtonComponent,
  DialogComponent,
  FieldRowComponent,
  IconComponent,
  LoadingSpinnerComponent,
} from '@shared/ui';
import { formatBytes } from '@shared/text';

@Component({
  selector: 'app-cache-nars-detail-drawer',
  standalone: true,
  imports: [CommonModule, DialogComponent, ButtonComponent, LoadingSpinnerComponent, IconComponent, FieldRowComponent, BadgeComponent],
  templateUrl: './cache-nars-detail-drawer.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
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

  readonly formatBytes = formatBytes;
}
