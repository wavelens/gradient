/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { Router, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { ProjectsService } from '@core/services/projects.service';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import {
  BadgeComponent,
  EmptyStateComponent,
  LoadMoreComponent,
  ButtonComponent,
  CardGridComponent,
  DialogComponent,
  FormFieldComponent,
  InputDirective,
  LoadStateComponent,
  NavCardComponent,
  PageLayoutComponent,
  SelectComponent,
} from '@gradient/ui/ui';
import { NameFieldsComponent, nameAvailability } from '@shared/forms';
import { ProjectSummary, PublicProjectSummary } from '@core/models';
import { links } from '@shared/routing';
import { pagedList, runReloadIntervalWhileTabVisible } from '@shared/paging';
import { EMPTY, merge } from 'rxjs';
import { PHASE_SEVERITY } from '@shared/evaluation';

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
    LoadStateComponent,
    PageLayoutComponent,
    FormFieldComponent,
    NavCardComponent,
    CardGridComponent,
    BadgeComponent,
    EmptyStateComponent,
    LoadMoreComponent,
    SelectComponent,
    NameFieldsComponent,
  ],
  templateUrl: './project-list.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class ProjectListComponent implements OnInit {
  protected readonly links = links;
  protected readonly phaseSeverity = PHASE_SEVERITY;
  readonly visibilityOptions = [
    { label: 'Private', value: false },
    { label: 'Public', value: true },
  ];

  private projectsService = inject(ProjectsService);
  protected authService = inject(AuthService);
  private config = inject(ConfigService);
  private notify = inject(NotifyService);
  private router = inject(Router);
  protected names = nameAvailability((name) => this.projectsService.checkProjectNameAvailable(name));

  get canCreateProject(): boolean {
    if (!this.authService.isAuthenticated()) return false;
    return this.config.canCreate(this.config.createProject, this.authService.user()?.superuser === true);
  }

  projects = pagedList<ProjectSummary>('projects', (page, options) => this.projectsService.getProjects(page, 50, options));
  showCreateDialog = signal(false);
  creating = signal(false);
  createError = signal<string | null>(null);
  createAttempted = signal(false);

  newProject = {
    name: '',
    display_name: '',
    description: '',
    public: false,
  };

  publicProjects = pagedList<PublicProjectSummary>('public projects', (page, options) => this.projectsService.getPublicProjects(page, 50, options));

  constructor() {
    runReloadIntervalWhileTabVisible(() => merge(
      this.authService.isAuthenticated() ? this.projects.refreshFirstPageSilently() : EMPTY,
      this.config.publicListing ? this.publicProjects.refreshFirstPageSilently() : EMPTY,
    ));
  }

  ngOnInit(): void {
    if (this.authService.isAuthenticated()) this.projects.reload();
    if (this.config.publicListing) this.publicProjects.reload();
  }

  openCreateDialog(): void {
    this.newProject = { name: '', display_name: '', description: '', public: false };
    this.names.reset();
    this.createError.set(null);
    this.createAttempted.set(false);
    this.showCreateDialog.set(true);
  }

  get filteredPublicProjects(): PublicProjectSummary[] {
    const ownedIds = new Set(this.projects.items().map((o) => o.id));
    return this.publicProjects.items().filter((o) => !ownedIds.has(o.id));
  }

  get missingNamesError(): string | null {
    const missing = !this.newProject.name || !this.newProject.display_name;
    return this.createAttempted() && missing ? 'Enter a display name and a name.' : null;
  }

  createProject(): void {
    if (this.creating() || this.names.blocksSubmit()) return;
    this.createAttempted.set(true);
    if (this.missingNamesError) return;

    this.creating.set(true);
    this.createError.set(null);
    const created = this.newProject.name;
    this.projectsService.createProject(this.newProject).subscribe({
      next: () => {
        this.creating.set(false);
        this.showCreateDialog.set(false);
        this.notify.success('Project created');
        this.router.navigateByUrl(links.project(created));
      },
      error: (error: unknown) => {
        this.createError.set(apiErrorMessage(error, 'Failed to create project.'));
        this.creating.set(false);
      },
    });
  }
}
