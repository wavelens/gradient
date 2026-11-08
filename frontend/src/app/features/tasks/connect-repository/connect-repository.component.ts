/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { ActivatedRoute, Router, RouterLink } from '@angular/router';
import { links } from '@shared/routing';
import { createMissingPermissionLoadStateError } from '@shared/access';
import { accessTo } from '@core/models/access.model';
import { injectProject } from '@core/resolvers/project-access.resolver';
import {
  ButtonComponent,
  FormFieldComponent,
  InputDirective,
  LoadStateComponent,
  MessageBannerComponent,
  PageLayoutComponent,
  SettingsSectionComponent,
  SubmitDirective,
} from '@gradient/ui/ui';
import { CacheSubscriptionItem, TaskRepositoryResponse } from '@core/models';
import { TasksService } from '@core/services/tasks.service';
import { ProjectsService } from '@core/services/projects.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import { NameFieldsComponent, fieldChecks, firstFieldError, nameAvailability } from '@shared/forms';
import { LabelHelpComponent } from '@shared/ui';
import { repositoryUrlError } from '@shared/text/repository-url';
import { slugify } from '@shared/text';
import { RepositoryAccessComponent } from '../task-repository/repository-access.component';
import { RepositoryEventsComponent } from '../task-repository/repository-events.component';

const RESERVED_TASK_NAMES = ['build-request'];
const DEFAULT_WILDCARD = 'packages.x86_64-linux.**';

export type ConnectStep = 'repository' | 'access' | 'events';
const STEPS: { id: ConnectStep; label: string }[] = [
  { id: 'repository', label: 'Repository' },
  { id: 'access', label: 'Access' },
  { id: 'events', label: 'Events and Status' },
];

function repositoryName(url: string): string {
  const path = url.trim().replace(/\?.*$/, '').replace(/\/+$/, '').replace(/\.git$/, '');
  return path.split(/[/:]/).pop() ?? '';
}

@Component({
  selector: 'app-connect-repository',
  standalone: true,
  imports: [
    FormsModule,
    RouterLink,
    ButtonComponent,
    FormFieldComponent,
    InputDirective,
    LoadStateComponent,
    MessageBannerComponent,
    PageLayoutComponent,
    SettingsSectionComponent,
    SubmitDirective,
    NameFieldsComponent,
    LabelHelpComponent,
    RepositoryAccessComponent,
    RepositoryEventsComponent,
  ],
  templateUrl: './connect-repository.component.html',
  styleUrl: './connect-repository.component.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class ConnectRepositoryComponent implements OnInit {
  protected readonly links = links;
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private tasks = inject(TasksService);
  private projects = inject(ProjectsService);
  private crumbs = inject(BreadcrumbsService);
  private notify = inject(NotifyService);
  private project = injectProject();

  readonly steps = STEPS;
  access = computed(() => accessTo(this.project(), 'createTask'));
  missingPermission = createMissingPermissionLoadStateError('createTask', 'project');
  projectName = '';
  step = signal<ConnectStep>('repository');
  stepIndex = computed(() => STEPS.findIndex((s) => s.id === this.step()));
  breadcrumb = computed(() => this.crumbs.project(this.projectName, { label: 'Connect repository' }));

  protected names = nameAvailability(
    (name) => this.tasks.checkTaskNameAvailable(this.projectName, name),
    RESERVED_TASK_NAMES,
  );
  form = { repository: '', display_name: '', name: '', wildcard: DEFAULT_WILDCARD };
  attempted = signal(false);
  creating = signal(false);
  createError = signal<string | null>(null);

  taskName = signal<string | null>(null);
  state = signal<TaskRepositoryResponse | null>(null);
  caches = signal<CacheSubscriptionItem[] | null>(null);

  get repositoryError(): string | null {
    const url = this.form.repository.trim();
    if (!url) return this.attempted() ? 'Enter the repository URL.' : null;
    return firstFieldError(url, fieldChecks.remoteRepository, (value) => repositoryUrlError(String(value)));
  }

  get missingNamesError(): string | null {
    return this.attempted() && (!this.form.name || !this.form.display_name) ? 'Enter a display name and a name.' : null;
  }

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') ?? '';
    if (!this.access().canEdit) return;
    const query = this.route.snapshot.queryParamMap;
    const task = query.get('task');
    if (task) {
      this.taskName.set(task);
      this.go(query.get('step') === 'events' ? 'events' : 'access');
    } else {
      this.loadCaches();
    }
  }

  repositoryChanged(url: string): void {
    const previous = repositoryName(this.form.repository);
    this.form.repository = url;
    if (this.form.display_name !== previous || this.form.name !== slugify(previous)) return;
    const name = repositoryName(url);
    this.form.display_name = name;
    this.form.name = slugify(name);
  }

  createTask(): void {
    if (this.creating() || this.names.blocksSubmit()) return;
    this.attempted.set(true);
    if (this.missingNamesError || this.repositoryError) return;
    this.creating.set(true);
    this.createError.set(null);
    this.tasks
      .createTask(this.projectName, {
        name: this.form.name,
        display_name: this.form.display_name,
        description: '',
        repository: this.form.repository.trim(),
        wildcard: this.form.wildcard.trim() || DEFAULT_WILDCARD,
      })
      .subscribe({
        next: () => {
          this.creating.set(false);
          this.notify.success('Task created');
          this.taskName.set(this.form.name);
          this.go('access');
        },
        error: (error: unknown) => {
          this.creating.set(false);
          this.createError.set(apiErrorMessage(error, 'Task creation failed'));
        },
      });
  }

  go(step: ConnectStep): void {
    this.step.set(step);
    const task = this.taskName();
    if (task) {
      this.router.navigate([], { relativeTo: this.route, queryParams: { task, step }, replaceUrl: true });
      this.loadState();
    }
  }

  next(): void {
    const following = STEPS[this.stepIndex() + 1];
    if (following) this.go(following.id);
  }

  back(): void {
    const previous = STEPS[this.stepIndex() - 1];
    if (previous && previous.id !== 'repository') this.go(previous.id);
  }

  loadState(): void {
    const task = this.taskName();
    if (!task) return;
    this.tasks.getRepository(this.projectName, task).subscribe({
      next: (state) => this.state.set(state),
      error: (error: unknown) => this.notify.failure('Failed to load repository', error),
    });
  }

  loadCaches(): void {
    this.projects.getSubscribedCaches(this.projectName).subscribe({
      next: (caches) => this.caches.set(caches),
      error: () => this.caches.set(null),
    });
  }

  missingCache(): boolean {
    const list = this.caches();
    return list !== null && !list.some((c) => c.status === 'active');
  }
}
