/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, inject, input, output, signal, OnInit } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActionsService } from '../../../core/services/actions.service';
import { ActionDelivery, ActionDeliveryDetail } from '../../../core/models/action.model';

@Component({
  selector: 'app-action-deliveries',
  standalone: true,
  imports: [CommonModule],
  templateUrl: './action-deliveries.component.html',
  styleUrl: './action-deliveries.component.scss',
})
export class ActionDeliveriesComponent implements OnInit {
  private actions = inject(ActionsService);

  org = input.required<string>();
  project = input.required<string>();
  actionId = input.required<string>();
  closed = output<void>();

  deliveries = signal<ActionDelivery[]>([]);
  loading = signal(true);
  loadError = signal<string | null>(null);
  expanded = signal<Map<string, ActionDeliveryDetail | 'loading' | 'error'>>(new Map());
  offset = signal(0);
  readonly limit = 50;
  hasMore = signal(false);

  ngOnInit() {
    this.loadPage();
  }

  loadPage() {
    this.loading.set(true);
    this.loadError.set(null);
    this.actions.listDeliveries(this.org(), this.project(), this.actionId(), this.limit, this.offset()).subscribe({
      next: (rows) => {
        this.deliveries.set(rows);
        this.hasMore.set(rows.length === this.limit);
        this.loading.set(false);
      },
      error: (e) => {
        this.loadError.set(e?.message ?? 'Failed to load deliveries');
        this.loading.set(false);
      },
    });
  }

  toggleRow(id: string) {
    const map = new Map(this.expanded());
    if (map.has(id)) {
      map.delete(id);
      this.expanded.set(map);
      return;
    }
    map.set(id, 'loading');
    this.expanded.set(map);
    this.actions.getDelivery(this.org(), this.project(), this.actionId(), id).subscribe({
      next: (detail) => {
        const m = new Map(this.expanded());
        m.set(id, detail);
        this.expanded.set(m);
      },
      error: () => {
        const m = new Map(this.expanded());
        m.set(id, 'error');
        this.expanded.set(m);
      },
    });
  }

  nextPage() {
    this.offset.update(o => o + this.limit);
    this.loadPage();
  }

  prevPage() {
    this.offset.update(o => Math.max(0, o - this.limit));
    this.loadPage();
  }

  close() { this.closed.emit(); }
}
