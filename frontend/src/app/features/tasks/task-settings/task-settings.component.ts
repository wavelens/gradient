/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, ElementRef, Injector, OnInit, afterNextRender, computed, inject, signal, viewChild, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { Observable } from 'rxjs';
import { TasksService } from '@core/services/tasks.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { ProjectsService } from '@core/services/projects.service';
import { ConfigService } from '@core/services/config.service';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import { ConfirmService } from '@shared/confirm';
import {
  ButtonComponent,
  CheckboxComponent,
  DialogComponent,
  FormFieldComponent,
  InputDirective,
  LoadStateComponent,
  MessageBannerComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
  SelectComponent,
  SettingsSectionComponent,
  SubmitDirective,
} from '@gradient/ui/ui';
import { LabelHelpComponent, entityToPickerOption, formatEntityLabel } from '@shared/ui';
import { AccessService, WritableDirective, ManagedDisableDirective, ServerConfigurationManagedBannerComponent, canOpenProjectSettings } from '@shared/access';
import { fieldChecks, injectRevealFirstInvalid, maxDisplayNameCharacterCount, unsavedChanges } from '@shared/forms';
import { wildcardError } from '../wildcard-error';
import { CanComponentDeactivate } from '@core/guards/unsaved-changes.guard';
import { TaskResponse, ConcurrencyPolicy, PatchTaskRequest } from '@core/models';
import { injectTaskAccess, injectTaskAccessData } from '@core/resolvers/inject-access';
import { links } from '@shared/routing';

interface TaskForm {
  display_name: string;
  description: string;
  wildcard: string;
  keep_evaluations: number | null;
  concurrency: ConcurrencyPolicy;
  sign_cache: boolean;
  wait_for_workers: boolean;
  retry_failed_builds: boolean;
  multi_node_evaluation: boolean;
  exec_enabled: boolean;
  only_evaluate: boolean;
}

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
    SelectComponent,
    CheckboxComponent,
    LoadStateComponent,
    WritableDirective,
    ManagedDisableDirective,
    PageLayoutComponent,
    FormFieldComponent,
    LabelHelpComponent,
    SettingsSectionComponent,
    MessageBannerComponent,
    ServerConfigurationManagedBannerComponent,
    RowComponent,
    RowListComponent,
    SubmitDirective,
  ],
  templateUrl: './task-settings.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './task-settings.component.scss',
})
export class TaskSettingsComponent implements OnInit, CanComponentDeactivate {
  protected readonly links = links;
  protected readonly maxDisplayNameCharacterCount = maxDisplayNameCharacterCount;
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private tasksService = inject(TasksService);
  private projectsService = inject(ProjectsService);
  private crumbs = inject(BreadcrumbsService);
  protected config = inject(ConfigService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);
  protected accessService = inject(AccessService);
  private injector = inject(Injector);
  private host = inject<ElementRef<HTMLElement>>(ElementRef);
  private revealFirstInvalid = injectRevealFirstInvalid();
  private saveArea = viewChild<ElementRef<HTMLElement>>('saveArea');

  access = injectTaskAccess();
  private resolvedTask = injectTaskAccessData();
  canOpenParentProjectSettings = computed(() => canOpenProjectSettings(this.resolvedTask()?.task));

  loading = signal(true);
  loadError = signal<unknown>(null);
  saving = signal(false);
  toggling = signal(false);
  transferring = signal(false);

  task = signal<TaskResponse | null>(null);
  showTransferDialog = signal(false);
  errorMessage = signal<string | null>(null);
  transferProjectName = '';
  transferError = signal<string | null>(null);
  transferTargetProjectOptions = signal<{ label: string; value: string }[]>([]);

  projectName = '';
  taskName = '';
  breadcrumb = computed(() => this.crumbs.task(this.projectName, this.taskName, { label: 'Settings' }));

  formData: TaskForm = {
    display_name: '',
    description: '',
    wildcard: '',
    keep_evaluations: 30,
    concurrency: 'auto',
    sign_cache: true,
    wait_for_workers: false,
    retry_failed_builds: true,
    multi_node_evaluation: false,
    exec_enabled: false,
    only_evaluate: false,
  };
  private savedForm: TaskForm = { ...this.formData };
  readonly unsaved = unsavedChanges(() => this.formData, 'Task Settings');

  get displayNameError(): string | null {
    return this.formData.display_name.trim() ? null : 'Enter a display name.';
  }

  get keepEvaluationsError(): string | null {
    const keep = this.formData.keep_evaluations;
    if (keep === null || String(keep).trim() === '') return 'Enter the number of evaluations to keep.';
    const max = this.config.maxKeepEvaluationsPerTask;
    const aboveServerLimit = max !== null && Number(keep) > max;
    return fieldChecks.positiveInteger(keep) ?? (aboveServerLimit ? `This server can keep at most ${max} evaluations per task.` : null);
  }

  get wildcardError(): string | null {
    return wildcardError(this.formData.wildcard);
  }

  get saveBlockedReason(): string | null {
    return this.displayNameError || this.wildcardError || this.keepEvaluationsError ? 'Fix the marked fields to save.' : null;
  }

  canDeactivate(): boolean | Observable<boolean> {
    return this.unsaved.canLeave();
  }

  concurrencyOptions: { label: string; value: ConcurrencyPolicy; disabled?: boolean }[] = [
    { label: 'Auto: abort the running evaluation once the new one builds, keep builds both need', value: 'auto' },
    { label: 'Hard Abort: cancel running evaluation and its in-flight builds', value: 'hard_abort' },
    { label: 'Soft Abort: mark current evaluation aborted, let in-flight builds finish', value: 'soft_abort' },
    { label: 'Skip: keep the running evaluation, discard the new trigger event', value: 'skip' },
    { label: 'All: run a new evaluation alongside the in-flight one', value: 'all' },
  ];

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.taskName = this.route.snapshot.paramMap.get('task') || '';
    this.loadTask();
  }

  loadTask(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.tasksService.getTaskInfo(this.projectName, this.taskName).subscribe({
      next: (task) => {
        if (task.name === 'build-request') {
          this.router.navigateByUrl(links.task(this.projectName, task.name));
          return;
        }
        this.task.set(task);
        this.crumbs.rememberTask(this.projectName, this.taskName, task.display_name);
        this.formData = {
          display_name: task.display_name,
          description: task.description,
          wildcard: task.wildcard,
          keep_evaluations: task.keep_evaluations,
          concurrency: task.concurrency,
          sign_cache: task.sign_cache,
          wait_for_workers: task.wait_for_workers,
          retry_failed_builds: task.retry_failed_builds,
          multi_node_evaluation: task.multi_node_evaluation,
          exec_enabled: task.exec_enabled,
          only_evaluate: task.only_evaluate,
        };
        this.savedForm = { ...this.formData };
        this.unsaved.mark();
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  saveSettings(): void {
    if (this.saving() || !this.accessService.isWritable(this.access())) return;
    if (this.saveBlockedReason) {
      this.revealFirstInvalid(() => this.host.nativeElement);
      return;
    }
    const saved = this.savedForm;
    const patch: PatchTaskRequest = Object.fromEntries(
      Object.entries(this.formData).filter(([key, value]) => saved[key as keyof TaskForm] !== value),
    );
    this.saving.set(true);
    this.errorMessage.set(null);
    this.tasksService.updateTask(this.projectName, this.taskName, patch).subscribe({
      next: () => {
        this.saving.set(false);
        this.notify.success('Task settings saved');
        this.loadTask();
      },
      error: (error) => {
        this.errorMessage.set(apiErrorMessage(error, 'Failed to save settings.'));
        this.saving.set(false);
        this.revealSaveResult();
      },
    });
  }

  private revealSaveResult(): void {
    afterNextRender(() => this.saveArea()?.nativeElement.scrollIntoView({ block: 'nearest' }), { injector: this.injector });
  }

  toggleActive(): void {
    const proj = this.task();
    if (!proj) return;

    this.toggling.set(true);
    const activate = !proj.active;
    const action = activate
      ? this.tasksService.activateTask(this.projectName, this.taskName)
      : this.tasksService.deactivateTask(this.projectName, this.taskName);

    action.subscribe({
      next: () => {
        this.toggling.set(false);
        this.notify.success(activate ? 'Task activated' : 'Task deactivated');
        this.task.set({ ...proj, active: activate });
      },
      error: (error: unknown) => {
        this.toggling.set(false);
        this.notify.failure(activate ? 'Failed to activate task' : 'Failed to deactivate task', error);
      },
    });
  }

  openTransferOwnershipDialogAndLoadTargetProjects(): void {
    this.showTransferDialog.set(true);
    this.projectsService.getProjects().subscribe({
      next: (res) => this.transferTargetProjectOptions.set(res.items
        .filter((project) => project.name !== this.projectName && !project.managed)
        .map((project) => entityToPickerOption(project))),
      error: (error: unknown) => this.transferError.set(apiErrorMessage(error, 'Failed to load projects.')),
    });
  }

  onTransferDialogHide(): void {
    this.transferProjectName = '';
    this.transferError.set(null);
  }

  transferOwnership(): void {
    const targetProject = this.transferProjectName;
    if (!targetProject) return;
    const targetLabel = this.transferTargetProjectOptions().find((option) => option.value === targetProject)?.label ?? targetProject;
    this.transferring.set(true);
    this.transferError.set(null);
    this.tasksService.transferOwnership(this.projectName, this.taskName, targetProject).subscribe({
      next: () => {
        this.transferring.set(false);
        this.showTransferDialog.set(false);
        this.notify.success(`Task transferred to ${targetLabel}`);
        this.router.navigateByUrl(links.task(targetProject, this.taskName));
      },
      error: (error) => {
        this.transferError.set(apiErrorMessage(error, 'Failed to transfer ownership.'));
        this.transferring.set(false);
      },
    });
  }

  deleteTask(): void {
    this.confirm.confirm({
      title: 'Delete task',
      target: formatEntityLabel({ display_name: this.task()?.display_name, name: this.taskName }),
      consequence: 'All evaluations, triggers, actions, and overrides for flake inputs disappear along with the task.',
      isIrreversible: true,
      confirmLabel: 'Delete',
      failure: 'Failed to delete the task.',
      action: () => this.tasksService.deleteTask(this.projectName, this.taskName),
    }).subscribe(() => {
      this.notify.success('Task deleted');
      this.router.navigateByUrl(links.project(this.projectName));
    });
  }
}
