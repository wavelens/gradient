/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { RouterModule } from '@angular/router';
import { forkJoin } from 'rxjs';
import { ProjectsService } from '@core/services/projects.service';
import { CachesService } from '@core/services/caches.service';
import { EmptyStateComponent, IconComponent, LoadingSpinnerComponent } from '@shared/ui';
import { Project, Cache } from '@core/models';

@Component({
  selector: 'app-dashboard',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    LoadingSpinnerComponent,
    EmptyStateComponent,
    IconComponent,
  ],
  templateUrl: './dashboard.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './dashboard.component.scss',
})
export class DashboardComponent implements OnInit {
  private projectsService = inject(ProjectsService);
  private cachesService = inject(CachesService);

  loading = signal(true);
  projects = signal<Project[]>([]);
  caches = signal<Cache[]>([]);

  ngOnInit(): void {
    this.loadDashboardData();
  }

  private loadDashboardData(): void {
    this.loading.set(true);

    forkJoin({
      projects: this.projectsService.getProjects(),
      caches: this.cachesService.getCaches(),
    }).subscribe({
      next: ({ projects, caches }) => {
        this.projects.set(projects.items);
        this.caches.set(caches.items);
        this.loading.set(false);
      },
      error: (error) => {
        console.error('Failed to load dashboard data:', error);
        this.loading.set(false);
      },
    });
  }

  get recentProjects() {
    return this.projects().slice(0, 5);
  }

  get recentCaches() {
    return this.caches().slice(0, 5);
  }
}
