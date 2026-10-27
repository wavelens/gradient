/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { ProjectsService } from '@core/services/projects.service';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import {
  BadgeComponent,
  ButtonComponent,
  CardGridComponent,
  DialogComponent,
  EmptyStateComponent,
  FormFieldComponent,
  IconComponent,
  InputDirective,
  LoadingSpinnerComponent,
  NameFieldComponent,
  NavCardComponent,
  PageLayoutComponent,
  SelectComponent,
} from '@gradient/ui/ui';
import { nameAvailability } from '@shared/forms';
import { Project, ProjectSummary } from '@core/models';

@Component({
  selector: 'app-project-list',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    DialogComponent,
    ButtonComponent,
    InputDirective,
    InputDirective,
    LoadingSpinnerComponent,
    EmptyStateComponent,
    IconComponent,
    PageLayoutComponent,
    FormFieldComponent,
    NavCardComponent,
    CardGridComponent,
    BadgeComponent,
    SelectComponent,
    NameFieldComponent,
  ],
  templateUrl: './project-list.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './project-list.component.scss',
})
export class ProjectListComponent implements OnInit {
  readonly visibilityOptions = [
    { label: 'Private', value: false },
    { label: 'Public', value: true },
  ];

  private projectsService = inject(ProjectsService);
  protected authService = inject(AuthService);
  private config = inject(ConfigService);
  protected names = nameAvailability((name) => this.projectsService.checkProjectNameAvailable(name));

  get canCreateProject(): boolean {
    if (!this.authService.isAuthenticated()) return false;
    return this.config.canCreate(this.config.createProject, this.authService.user()?.superuser === true);
  }

  loading = signal(true);
  projects = signal<ProjectSummary[]>([]);
  projectsTotal = signal(0);
  projectsPage = signal(1);
  showCreateDialog = signal(false);
  creating = signal(false);
  createError = signal<string | null>(null);

  newProject = {
    name: '',
    display_name: '',
    description: '',
    public: false,
  };

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
    this.names.reset();
    this.createError.set(null);
    this.showCreateDialog.set(true);
  }

  onProjectDisplayNameChange(value: string): void {
    const name = this.names.suggest(value);
    if (name !== null) this.newProject.name = name;
  }

  onProjectNameChange(name: string): void {
    this.newProject.name = name;
    this.names.check(name);
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
