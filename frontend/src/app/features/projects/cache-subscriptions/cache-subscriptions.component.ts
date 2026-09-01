/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { ProjectsService } from '@core/services/projects.service';
import { CachesService } from '@core/services/caches.service';
import { ProjectAccessService } from '@core/services/project-access.service';
import {
  AutoCompleteComponent,
  ButtonComponent,
  DialogComponent,
  EmptyStateComponent,
  FormFieldComponent,
  IconComponent,
  LoadingSpinnerComponent,
  PageLayoutComponent,
} from '@shared/ui';
import { WritableDirective, ManagedDisableDirective } from '@shared/access';
import { AccessState } from '@core/models';

@Component({
  selector: 'app-cache-subscriptions',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    DialogComponent,
    ButtonComponent,
    AutoCompleteComponent,
    LoadingSpinnerComponent,
    WritableDirective,
    ManagedDisableDirective,
    IconComponent,
    PageLayoutComponent,
    FormFieldComponent,
    EmptyStateComponent,
  ],
  templateUrl: './cache-subscriptions.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './cache-subscriptions.component.scss',
})
export class CacheSubscriptionsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private projectsService = inject(ProjectsService);
  private cachesService = inject(CachesService);
  private projectAccess = inject(ProjectAccessService);

  access = signal<AccessState>({ managed: false, canEdit: false, canTrigger: false });

  loading = signal(true);
  subscribing = signal(false);
  unsubscribingId = signal<string | null>(null);
  showSubscribeDialog = signal(false);
  errorMessage = signal<string | null>(null);

  projectName = '';
  projectDisplayName = signal('');
  caches = signal<{ id: string; name: string }[]>([]);
  newCacheName = '';
  cacheSuggestions = signal<string[]>([]);
  private availableCacheNames: string[] = [];

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.projectAccess.forProject(this.projectName).then((s) => this.access.set(s));
    this.projectsService.getProject(this.projectName).subscribe({
      next: (project) => this.projectDisplayName.set(project.display_name),
      error: () => {},
    });
    this.loadCaches();
  }

  loadCaches(): void {
    this.loading.set(true);
    this.projectsService.getSubscribedCaches(this.projectName).subscribe({
      next: (list) => {
        this.caches.set(list);
        this.loading.set(false);
      },
      error: () => this.loading.set(false),
    });
  }

  openSubscribeDialog(): void {
    this.newCacheName = '';
    this.errorMessage.set(null);
    this.cacheSuggestions.set([]);
    this.showSubscribeDialog.set(true);
    this.loadAvailableCaches();
  }

  private loadAvailableCaches(): void {
    const subscribedNames = new Set(this.caches().map((c) => c.name));
    this.cachesService.getCaches(1, 100).subscribe({
      next: (own) => {
        this.cachesService.getPublicCaches().subscribe({
          next: (pub) => {
            const all = [...own.items, ...pub];
            const seen = new Set<string>();
            this.availableCacheNames = all
              .filter((c) => !subscribedNames.has(c.name) && !seen.has(c.name) && seen.add(c.name))
              .map((c) => c.name);
          },
          error: () => {
            this.availableCacheNames = own.items
              .filter((c) => !subscribedNames.has(c.name))
              .map((c) => c.name);
          },
        });
      },
      error: () => {},
    });
  }

  onCacheSearch(event: { query: string }): void {
    const q = event.query.toLowerCase();
    this.cacheSuggestions.set(
      this.availableCacheNames.filter((name) => name.toLowerCase().includes(q))
    );
  }

  subscribeCache(): void {
    const name = this.newCacheName.trim();
    if (!name) return;
    this.subscribing.set(true);
    this.errorMessage.set(null);
    this.projectsService.subscribeCache(this.projectName, name).subscribe({
      next: () => {
        this.subscribing.set(false);
        this.showSubscribeDialog.set(false);
        this.loadCaches();
      },
      error: (err) => {
        this.errorMessage.set(err?.error?.message || err?.message || 'Cache not found or already subscribed.');
        this.subscribing.set(false);
      },
    });
  }

  unsubscribeCache(cache: { id: string; name: string }): void {
    this.unsubscribingId.set(cache.id);
    this.projectsService.unsubscribeCache(this.projectName, cache.name).subscribe({
      next: () => {
        this.unsubscribingId.set(null);
        this.loadCaches();
      },
      error: () => this.unsubscribingId.set(null),
    });
  }
}
