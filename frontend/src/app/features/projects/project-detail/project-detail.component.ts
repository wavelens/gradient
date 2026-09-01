/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnDestroy, OnInit, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { Subject, forkJoin, EMPTY } from 'rxjs';
import { debounceTime, switchMap } from 'rxjs/operators';
import { AuthService } from '@core/services/auth.service';
import { ProjectsService } from '@core/services/projects.service';
import { TasksService } from '@core/services/tasks.service';
import {
  ButtonComponent,
  DialogComponent,
  EmptyStateComponent,
  EvalStatusBadgeComponent,
  IconComponent,
  InputDirective,
  LabelHelpComponent,
  LoadingSpinnerComponent,
  PageLayoutComponent,
} from '@shared/ui';
import { slugify } from '@shared/text';
import { Project, Task } from '@core/models';

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
    PageLayoutComponent,
  ],
  templateUrl: './project-detail.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './project-detail.component.scss',
})
export class ProjectDetailComponent implements OnInit, OnDestroy {
  private route = inject(ActivatedRoute);
  protected authService = inject(AuthService);
  private projectsService = inject(ProjectsService);
  private tasksService = inject(TasksService);
  private nameCheck$ = new Subject<string>();

  loading = signal(true);
  project = signal<Project | null>(null);
  tasks = signal<Task[]>([]);
  tasksTotal = signal(0);
  tasksPage = signal(1);
  showCreateDialog = signal(false);
  creating = signal(false);
  createError = signal<string | null>(null);
  nameCheckState = signal<'idle' | 'invalid' | 'reserved' | 'checking' | 'available' | 'taken'>('idle');

  projectName = '';

  newTask = {
    name: '',
    display_name: '',
    description: '',
    repository: '',
    wildcard: 'packages.x86_64-linux.*',
  };

  protected taskNameEditedByUser = false;

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.loadProjectData();
    this.nameCheck$.pipe(
      debounceTime(400),
      switchMap((name) => name ? this.tasksService.checkTaskNameAvailable(this.projectName, name) : EMPTY),
    ).subscribe((available) => {
      this.nameCheckState.set(available ? 'available' : 'taken');
    });
  }

  ngOnDestroy(): void {
    this.nameCheck$.complete();
  }

  loadProjectData(): void {
    this.loading.set(true);

    forkJoin({
      project: this.projectsService.getProject(this.projectName),
      tasks: this.tasksService.getTasks(this.projectName, this.tasksPage()),
    }).subscribe({
      next: ({ project, tasks }) => {
        this.project.set(project);
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
    this.newTask = { name: '', display_name: '', description: '', repository: '', wildcard: 'packages.x86_64-linux.*' };
    this.taskNameEditedByUser = false;
    this.nameCheckState.set('idle');
    this.createError.set(null);
    this.showCreateDialog.set(true);
  }

  onTaskDisplayNameChange(value: string): void {
    if (!this.taskNameEditedByUser) {
      const slug = slugify(value);
      this.newTask.name = slug;
      this.onTaskNameChange(slug);
    }
  }

  onTaskNameUserInput(): void {
    this.taskNameEditedByUser = true;
  }

  onTaskNameChange(name: string): void {
    if (!name) { this.nameCheckState.set('idle'); this.nameCheck$.next(''); return; }
    if (!/^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/.test(name)) {
      this.nameCheckState.set('invalid');
      this.nameCheck$.next(''); // cancel any pending debounce without making an API call
      return;
    }
    if (RESERVED_TASK_NAMES.includes(name.toLowerCase())) {
      this.nameCheckState.set('reserved');
      this.nameCheck$.next('');
      return;
    }
    this.nameCheckState.set('checking');
    this.nameCheck$.next(name);
  }

  get wildcardInvalid(): boolean {
    const w = this.newTask.wildcard.trim();
    if (!w) return false; // empty means use default - not invalid
    const parts = w.split(',').map((p) => p.trim());
    return parts.some((p) => !p || p.startsWith('.') || /\s/.test(p));
  }

  createTask(): void {
    if (!this.newTask.name || !this.newTask.display_name || !this.newTask.repository) {
      return;
    }
    if (RESERVED_TASK_NAMES.includes(this.newTask.name.trim().toLowerCase())) {
      this.nameCheckState.set('reserved');
      return;
    }

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
