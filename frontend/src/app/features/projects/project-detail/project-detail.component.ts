/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { forkJoin } from 'rxjs';
import { AuthService } from '@core/services/auth.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { ProjectsService } from '@core/services/projects.service';
import { TasksService } from '@core/services/tasks.service';
import { StarsService } from '@core/services/stars.service';
import {
  ButtonComponent,
  CardGridComponent,
  DialogComponent,
  EmptyStateComponent,
  FormFieldComponent,
  IconComponent,
  InputDirective,
  LoadingSpinnerComponent,
  NavCardComponent,
  PageLayoutComponent,
  SubmitDirective,
} from '@gradient/ui/ui';
import { EvalStatusBadgeComponent, LabelHelpComponent, StarButtonComponent } from '@shared/ui';
import { repositoryUrlError } from '@shared/text';
import { NameFieldsComponent, nameAvailability } from '@shared/forms';
import { StarTarget, ProjectResponse, TaskResponse } from '@core/models';

const RESERVED_TASK_NAMES = ['build-request'];

@Component({
  selector: 'app-project-detail',
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
    LabelHelpComponent,
    EvalStatusBadgeComponent,
    IconComponent,
    CardGridComponent,
    NavCardComponent,
    PageLayoutComponent,
    FormFieldComponent,
    StarButtonComponent,
    SubmitDirective,
    NameFieldsComponent,
  ],
  templateUrl: './project-detail.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './project-detail.component.scss',
})
export class ProjectDetailComponent implements OnInit {
  private route = inject(ActivatedRoute);
  protected authService = inject(AuthService);
  private projectsService = inject(ProjectsService);
  private tasksService = inject(TasksService);
  private stars = inject(StarsService);
  private crumbs = inject(BreadcrumbsService);
  protected names = nameAvailability(
    (name) => this.tasksService.checkTaskNameAvailable(this.projectName, name),
    RESERVED_TASK_NAMES,
  );

  loading = signal(true);
  project = signal<ProjectResponse | null>(null);
  tasks = signal<TaskResponse[]>([]);
  tasksTotal = signal(0);
  tasksPage = signal(1);
  showCreateDialog = signal(false);
  creating = signal(false);
  createError = signal<string | null>(null);
  starred = signal(false);

  projectName = '';
  breadcrumb = computed(() => this.crumbs.project(this.projectName));
  starTarget: StarTarget = { kind: 'project', project: '' };

  newTask = {
    name: '',
    display_name: '',
    description: '',
    repository: '',
    wildcard: 'packages.x86_64-linux.**',
  };

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.starTarget = { kind: 'project', project: this.projectName };
    this.stars.starred(this.starTarget).subscribe((starred) => this.starred.set(starred));
    this.loadProjectData();
  }

  loadProjectData(): void {
    this.loading.set(true);

    forkJoin({
      project: this.projectsService.getProject(this.projectName),
      tasks: this.tasksService.getTasks(this.projectName, this.tasksPage()),
    }).subscribe({
      next: ({ project, tasks }) => {
        this.project.set(project);
        this.crumbs.rememberProject(this.projectName, project.display_name);
        this.tasks.set(tasks.items);
        this.tasksTotal.set(tasks.total);
        this.tasksPage.set(tasks.page);
        this.loading.set(false);
      },
      error: (error) => {
        console.error('Failed to load project data:', error);
        this.loading.set(false);
      },
    });
  }

  openCreateDialog(): void {
    this.newTask = { name: '', display_name: '', description: '', repository: '', wildcard: 'packages.x86_64-linux.**' };
    this.names.reset();
    this.createError.set(null);
    this.showCreateDialog.set(true);
  }

  get wildcardInvalid(): boolean {
    const w = this.newTask.wildcard.trim();
    if (!w) return false; // empty means use default - not invalid
    const parts = w.split(',').map((p) => p.trim());
    return parts.some((p) => !p || p.startsWith('.') || /\s/.test(p));
  }

  get repositoryError(): string | null {
    return repositoryUrlError(this.newTask.repository.trim());
  }

  createTask(): void {
    if (!this.newTask.name || !this.newTask.display_name || !this.newTask.repository || this.names.blocksSubmit()) {
      return;
    }
    if (this.repositoryError || this.wildcardInvalid) return;

    this.creating.set(true);
    this.createError.set(null);
    this.tasksService
      .createTask(this.projectName, this.newTask)
      .subscribe({
        next: () => {
          this.creating.set(false);
          this.showCreateDialog.set(false);
          this.loadProjectData();
        },
        error: (error) => {
          this.createError.set(error?.message || 'Failed to create task.');
          this.creating.set(false);
        },
      });
  }

}
