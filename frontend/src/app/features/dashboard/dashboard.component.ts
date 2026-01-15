/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { RouterModule } from '@angular/router';
import { forkJoin } from 'rxjs';
import { OrganizationsService } from '@core/services/organizations.service';
import { CachesService } from '@core/services/caches.service';
import { StatCardComponent } from '@shared/components/stat-card/stat-card.component';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { EmptyStateComponent } from '@shared/components/empty-state/empty-state.component';
import { Organization, Cache } from '@core/models';

@Component({
  selector: 'app-dashboard',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    StatCardComponent,
    LoadingSpinnerComponent,
    EmptyStateComponent,
  ],
  templateUrl: './dashboard.component.html',
  styleUrl: './dashboard.component.scss',
})
export class DashboardComponent implements OnInit {
  private organizationsService = inject(OrganizationsService);
  private cachesService = inject(CachesService);

  loading = signal(true);
  organizations = signal<Organization[]>([]);
  caches = signal<Cache[]>([]);

  ngOnInit(): void {
    this.loadDashboardData();
  }

  private loadDashboardData(): void {
    this.loading.set(true);

    forkJoin({
      organizations: this.organizationsService.getOrganizations(),
      caches: this.cachesService.getCaches(),
    }).subscribe({
      next: ({ organizations, caches }) => {
        this.organizations.set(organizations);
        this.caches.set(caches);
        this.loading.set(false);
      },
      error: (error) => {
        console.error('Failed to load dashboard data:', error);
        this.loading.set(false);
      },
    });
  }

  get recentOrganizations() {
    return this.organizations().slice(0, 5);
  }

  get recentCaches() {
    return this.caches().slice(0, 5);
  }
}
