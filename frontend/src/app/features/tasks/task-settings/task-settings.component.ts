/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { TasksService } from '@core/services/tasks.service';
import { ProjectsService } from '@core/services/projects.service';
import {
  AutoCompleteComponent,
  ButtonComponent,
  CheckboxComponent,
  DialogComponent,
  FormFieldComponent,
  InputDirective,
  LabelHelpComponent,
  LoadingSpinnerComponent,
  MessageBannerComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
  SelectComponent,
  SettingsSectionComponent,
} from '@shared/ui';
import { WritableDirective, ManagedDisableDirective } from '@shared/access';
import { ConcurrencyPolicy, Task } from '@core/models';
import { injectTaskAccess } from '@core/resolvers/inject-access';

@Component({
  selector: 'app-task-settings',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    DialogComponent,
    ButtonComponent,
    InputDirective,
    InputDirective,
    AutoCompleteComponent,
    SelectComponent,
    CheckboxComponent,
    LoadingSpinnerComponent,
    WritableDirective,
    ManagedDisableDirective,
    PageLayoutComponent,
    FormFieldComponent,
    LabelHelpComponent,
    SettingsSectionComponent,
    MessageBannerComponent,
    RowComponent,
    RowListComponent,
  ],
  templateUrl: './task-settings.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './task-settings.component.scss',
})
export class TaskSettingsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private tasksService = inject(TasksService);
  private projectsService = inject(ProjectsService);

  access = injectTaskAccess();

  loading = signal(true);
  saving = signal(false);
  deleting = signal(false);
  toggling = signal(false);
  transferring = signal(false);

  task = signal<Task | null>(null);
  showDeleteDialog = signal(false);
  showTransferDialog = signal(false);
  errorMessage = signal<string | null>(null);
  saveSuccess = signal(false);
  transferProjectName = '';
  transferError = signal<string | null>(null);
  transferSuccess = signal(false);
  transferProjectSuggestions = signal<string[]>([]);

  projectName = '';
  projectDisplayName = signal('');
  taskName = '';

  formData: {
    display_name: string;
    description: string;
    repository: string;
    wildcard: string;
    keep_evaluations: number;
    concurrency: ConcurrencyPolicy;
    sign_cache: boolean;
  } = {
    display_name: '',
    description: '',
    repository: '',
    wildcard: '',
    keep_evaluations: 30,
    concurrency: 'soft_abort',
    sign_cache: true,
  };

  concurrencyOptions: { label: string; value: ConcurrencyPolicy; disabled?: boolean }[] = [
    { label: 'Hard Abort: cancel running evaluation and its in-flight builds', value: 'hard_abort' },
    { label: 'Soft Abort: mark current evaluation aborted, let in-flight builds finish', value: 'soft_abort' },
    { label: 'Skip: keep the running evaluation, discard the new trigger event', value: 'skip' },
    { label: 'All: run a new evaluation alongside the in-flight one', value: 'all' },
  ];

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.taskName = this.route.snapshot.paramMap.get('task') || '';
    this.projectsService.getProject(this.projectName).subscribe({
      next: (project) => this.projectDisplayName.set(project.display_name),
      error: () => {},
    });
    this.loadTask();
  }

  loadTask(): void {
    this.loading.set(true);
    this.tasksService.getTaskInfo(this.projectName, this.taskName).subscribe({
      next: (task) => {
        if (task.name === 'build-request') {
          this.router.navigate(['/project', this.projectName, 'task', task.name]);
          return;
        }
        this.task.set(task);
        this.formData = {
          display_name: task.display_name,
          description: task.description,
          repository: task.repository,
          wildcard: task.wildcard,
          keep_evaluations: task.keep_evaluations,
          concurrency: task.concurrency,
          sign_cache: task.sign_cache,
        };
        this.loading.set(false);
      },
      error: (error) => {
        console.error('Failed to load task:', error);
        this.loading.set(false);
      },
    });
  }

  saveSettings(): void {
    this.saving.set(true);
    this.errorMessage.set(null);
    this.saveSuccess.set(false);
    this.tasksService.updateTask(this.projectName, this.taskName, this.formData).subscribe({
      next: () => {
        this.saving.set(false);
        this.saveSuccess.set(true);
        this.loadTask();
      },
      error: (error) => {
        this.errorMessage.set(error.message || 'Failed to save settings.');
        this.saving.set(false);
      },
    });
  }

  toggleActive(): void {
    const proj = this.task();
    if (!proj) return;

    this.toggling.set(true);
    const action = proj.active
      ? this.tasksService.deactivateTask(this.projectName, this.taskName)
      : this.tasksService.activateTask(this.projectName, this.taskName);

    action.subscribe({
      next: () => {
        this.toggling.set(false);
        this.loadTask();
      },
      error: (error) => {
        console.error('Failed to toggle task status:', error);
        this.toggling.set(false);
      },
    });
  }

  onTransferProjectSearch(event: { query: string }): void {
    const q = event.query.trim().toLowerCase();
    this.projectsService.getProjects().subscribe({
      next: (res) => {
        const names = res.items
          .map((o) => o.name)
          .filter((n) => n !== this.projectName && (!q || n.toLowerCase().includes(q)));
        this.transferProjectSuggestions.set(names);
      },
      error: () => this.transferProjectSuggestions.set([]),
    });
  }

  onTransferDialogHide(): void {
    this.transferProjectName = '';
    this.transferError.set(null);
    this.transferSuccess.set(false);
  }

  transferOwnership(): void {
    if (!this.transferProjectName.trim()) return;
    this.transferring.set(true);
    this.transferError.set(null);
    this.transferSuccess.set(false);
    this.tasksService.transferOwnership(this.projectName, this.taskName, this.transferProjectName.trim()).subscribe({
      next: () => {
        this.transferring.set(false);
        this.transferSuccess.set(true);
        const targetProject = this.transferProjectName.trim();
        this.transferProjectName = '';
        setTimeout(() => {
          this.showTransferDialog.set(false);
          this.router.navigate(['/project', targetProject]);
        }, 1500);
      },
      error: (error) => {
        this.transferError.set(error.message || 'Failed to transfer ownership.');
        this.transferring.set(false);
      },
    });
  }

  deleteTask(): void {
    this.deleting.set(true);
    this.tasksService.deleteTask(this.projectName, this.taskName).subscribe({
      next: () => {
        this.router.navigate(['/project', this.projectName]);
      },
      error: (error) => {
        console.error('Failed to delete task:', error);
        this.deleting.set(false);
        this.showDeleteDialog.set(false);
      },
    });
  }
}
