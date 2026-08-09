/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnDestroy, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { Subject, EMPTY } from 'rxjs';
import { debounceTime, switchMap } from 'rxjs/operators';
import { DialogModule } from 'primeng/dialog';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';
import { TextareaModule } from 'primeng/textarea';
import { ProjectsService } from '@core/services/projects.service';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { EmptyStateComponent } from '@shared/components/empty-state/empty-state.component';
import { slugify } from '@shared/text';
import { Project } from '@core/models';

@Component({
  selector: 'app-project-list',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    DialogModule,
    ButtonModule,
    InputTextModule,
    TextareaModule,
    LoadingSpinnerComponent,
    EmptyStateComponent,
  ],
  templateUrl: './project-list.component.html',
  styleUrl: './project-list.component.scss',
})
export class ProjectListComponent implements OnInit, OnDestroy {
  private projectsService = inject(ProjectsService);
  protected authService = inject(AuthService);
  private config = inject(ConfigService);
  private nameCheck$ = new Subject<string>();

  get canCreateProject(): boolean {
    if (!this.authService.isAuthenticated()) return false;
    return this.config.canCreate(this.config.createProject, this.authService.user()?.superuser === true);
  }

  loading = signal(true);
  projects = signal<Project[]>([]);
  projectsTotal = signal(0);
  projectsPage = signal(1);
  showCreateDialog = signal(false);
  creating = signal(false);
  createError = signal<string | null>(null);
  nameCheckState = signal<'idle' | 'invalid' | 'checking' | 'available' | 'taken'>('idle');

  newProject = {
    name: '',
    display_name: '',
    description: '',
    public: false,
  };

  protected projectNameEditedByUser = false;

  publicProjects = signal<Project[]>([]);
  publicTotal = signal(0);
  publicPage = signal(1);
  publicLoading = signal(false);

  ngOnInit(): void {
    if (this.authService.isAuthenticated()) {
      this.loadProjects();
    } else {
      this.loading.set(false);
    }
    this.loadPublicProjects();
    this.nameCheck$.pipe(
      debounceTime(400),
      switchMap((name) => name ? this.projectsService.checkProjectNameAvailable(name) : EMPTY),
    ).subscribe((available) => {
      this.nameCheckState.set(available ? 'available' : 'taken');
    });
  }

  ngOnDestroy(): void {
    this.nameCheck$.complete();
  }

  loadProjects(page = this.projectsPage()): void {
    this.loading.set(true);
    this.projectsService.getProjects(page).subscribe({
      next: (result) => {
        this.projects.set(result.items);
        this.projectsTotal.set(result.total);
        this.projectsPage.set(result.page);
        this.loading.set(false);
      },
      error: (error) => {
        console.error('Failed to load projects:', error);
        this.loading.set(false);
      },
    });
  }

  loadPublicProjects(page = this.publicPage()): void {
    this.publicLoading.set(true);
    this.projectsService.getPublicProjects(page).subscribe({
      next: (result) => {
        this.publicProjects.set(result.items);
        this.publicTotal.set(result.total);
        this.publicPage.set(result.page);
        this.publicLoading.set(false);
      },
      error: () => this.publicLoading.set(false),
    });
  }

  openCreateDialog(): void {
    this.newProject = { name: '', display_name: '', description: '', public: false };
    this.projectNameEditedByUser = false;
    this.nameCheckState.set('idle');
    this.createError.set(null);
    this.showCreateDialog.set(true);
  }

  onProjectDisplayNameChange(value: string): void {
    if (!this.projectNameEditedByUser) {
      const slug = slugify(value);
      this.newProject.name = slug;
      this.onProjectNameChange(slug);
    }
  }

  onProjectNameUserInput(): void {
    this.projectNameEditedByUser = true;
  }

  onProjectNameChange(name: string): void {
    if (!name) { this.nameCheckState.set('idle'); this.nameCheck$.next(''); return; }
    if (!/^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/.test(name)) {
      this.nameCheckState.set('invalid');
      this.nameCheck$.next(''); // cancel any pending debounce without making an API call
      return;
    }
    this.nameCheckState.set('checking');
    this.nameCheck$.next(name);
  }

  get filteredPublicProjects(): Project[] {
    const ownedIds = new Set(this.projects().map((o) => o.id));
    return this.publicProjects().filter((o) => !ownedIds.has(o.id));
  }

  createProject(): void {
    if (!this.newProject.name || !this.newProject.display_name) {
      return;
    }

    this.creating.set(true);
    this.createError.set(null);
    this.projectsService.createProject(this.newProject).subscribe({
      next: () => {
        this.creating.set(false);
        this.showCreateDialog.set(false);
        this.loadProjects();
      },
      error: (error) => {
        this.createError.set(error?.message || 'Failed to create project.');
        this.creating.set(false);
      },
    });
  }
}
