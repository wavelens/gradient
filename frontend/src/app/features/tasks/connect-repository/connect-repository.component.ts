/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, ElementRef, OnInit, computed, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { ActivatedRoute, Router, RouterLink } from '@angular/router';
import { links } from '@shared/routing';
import { createMissingPermissionLoadStateError, isViewerProjectMember } from '@shared/access';
import { accessTo } from '@core/models/access.model';
import { injectProject } from '@core/resolvers/project-access.resolver';
import {
  ButtonComponent,
  CheckboxComponent,
  CopyFieldComponent,
  FormFieldComponent,
  InputDirective,
  LoadStateComponent,
  MessageBannerComponent,
  PageLayoutComponent,
  SettingsSectionComponent,
  SubmitDirective,
} from '@gradient/ui/ui';
import { CacheSubscriptionItem, IntegrationSummaryResponse, TriggerConfig } from '@core/models';
import { ProjectNoActiveWritableCacheSubscriptionWarningComponent, hasActiveCacheSubscriptionWithWriteAccess } from '@features/projects/project-no-active-writable-cache-subscription-warning.component';
import { TasksService } from '@core/services/tasks.service';
import { ProjectsService } from '@core/services/projects.service';
import { TriggersService } from '@core/services/triggers.service';
import { IntegrationsService } from '@core/services/integrations.service';
import { ConfigService } from '@core/services/config.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import { NameFieldsComponent, fieldChecks, firstFieldError, injectRevealFirstInvalid, nameAvailability } from '@shared/forms';
import { wildcardError } from '../wildcard-error';
import { LabelHelpComponent } from '@shared/ui';
import { repositoryUrlError } from '@shared/text/repository-url';
import { slugify } from '@shared/text';

export const RESERVED_TASK_NAMES = ['build-request'];
const DEFAULT_WILDCARD = 'packages.x86_64-linux.**';
const DEFAULT_POLLING_INTERVAL_SECS = 300;

export type ConnectStep = 'repository' | 'access' | 'triggers';
const STEPS: { id: ConnectStep; label: string }[] = [
  { id: 'repository', label: 'Repository' },
  { id: 'access', label: 'Access' },
  { id: 'triggers', label: 'Triggers and Settings' },
];

interface SuggestedTriggerChoice {
  config: TriggerConfig;
  selected: boolean;
}

function repositoryName(url: string): string {
  const path = url.trim().replace(/\?.*$/, '').replace(/\/+$/, '').replace(/\.git$/, '');
  return path.split(/[/:]/).pop() ?? '';
}

export function buildTaskCreationTriggersFromWizardChoices(choices: {
  branch: string;
  polling: boolean;
  cron: string;
  suggestions: SuggestedTriggerChoice[];
}): TriggerConfig[] {
  const branch = choices.branch.trim() || null;
  const triggers: TriggerConfig[] = [];
  if (choices.polling) triggers.push({ type: 'polling', interval_secs: DEFAULT_POLLING_INTERVAL_SECS, branch });
  for (const { config, selected } of choices.suggestions) {
    if (!selected || config.type === 'polling' || config.type === 'time') continue;
    triggers.push(branch ? { ...config, branches: [branch] } : config);
  }
  const cron = choices.cron.trim();
  if (cron) triggers.push({ type: 'time', cron, branch });
  return triggers;
}

@Component({
  selector: 'app-connect-repository',
  standalone: true,
  imports: [
    FormsModule,
    ProjectNoActiveWritableCacheSubscriptionWarningComponent,
    RouterLink,
    ButtonComponent,
    CheckboxComponent,
    CopyFieldComponent,
    FormFieldComponent,
    InputDirective,
    LoadStateComponent,
    MessageBannerComponent,
    PageLayoutComponent,
    SettingsSectionComponent,
    SubmitDirective,
    NameFieldsComponent,
    LabelHelpComponent,
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
  private triggers = inject(TriggersService);
  private integrations = inject(IntegrationsService);
  private crumbs = inject(BreadcrumbsService);
  private notify = inject(NotifyService);
  private host = inject<ElementRef<HTMLElement>>(ElementRef);
  private revealFirstInvalid = injectRevealFirstInvalid();
  private project = injectProject();
  protected readonly config = inject(ConfigService);

  readonly steps = STEPS;
  access = computed(() => accessTo(this.project(), 'createTask'));
  missingPermission = computed(() => createMissingPermissionLoadStateError('project', isViewerProjectMember(this.project()), 'createTask'));
  projectName = '';
  step = signal<ConnectStep>('repository');
  stepIndex = computed(() => STEPS.findIndex((s) => s.id === this.step()));
  breadcrumb = computed(() => this.crumbs.project(this.projectName, { label: 'Connect repository' }));

  protected names = nameAvailability(
    (name) => this.tasks.checkTaskNameAvailable(this.projectName, name),
    RESERVED_TASK_NAMES,
  );
  form = { repository: '', display_name: '', name: '', wildcard: DEFAULT_WILDCARD, branch: '', polling: true, cron: '', exec_enabled: false };
  private autofilledDisplayName = '';
  attempted = signal(false);
  creating = signal(false);
  createError = signal<string | null>(null);
  caches = signal<CacheSubscriptionItem[] | null>(null);

  deployKey = signal<string | null>(null);
  testing = signal(false);
  repositoryAccessTestPassedForCurrentUrl = signal(false);
  testError = signal<string | null>(null);
  suggestions = signal<SuggestedTriggerChoice[]>([]);
  private inboundIntegrations = signal<IntegrationSummaryResponse[]>([]);

  get repositoryError(): string | null {
    const url = this.form.repository.trim();
    if (!url) return this.attempted() ? 'Enter the repository URL.' : null;
    return firstFieldError(url, fieldChecks.remoteRepository, (value) => repositoryUrlError(String(value)));
  }

  get wildcardError(): string | null {
    const wildcard = this.form.wildcard.trim();
    return wildcard ? wildcardError(wildcard) : null;
  }

  get cronError(): string | null {
    const cron = this.form.cron.trim();
    if (!cron) return null;
    const fields = cron.split(/\s+/).length;
    return fields >= 5 && fields <= 7 ? null : 'Enter 5, 6, or 7 cron fields separated by spaces.';
  }

  usesSsh(): boolean {
    return !/^(https?|git):\/\//.test(this.form.repository.trim());
  }

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') ?? '';
    if (!this.access().canEdit) return;
    this.loadCaches();
  }

  repositoryChanged(url: string): void {
    this.form.repository = url;
    this.repositoryAccessTestPassedForCurrentUrl.set(false);
    this.testError.set(null);
    const previous = this.autofilledDisplayName;
    if (this.form.display_name !== previous || this.form.name !== slugify(previous)) return;
    const name = this.repositoryError ? '' : repositoryName(url);
    this.autofilledDisplayName = name;
    this.form.display_name = name;
    this.form.name = slugify(name);
  }

  continueFromRepository(): void {
    if (this.names.blocksSubmit()) return;
    this.attempted.set(true);
    if (!this.form.name || !this.form.display_name || this.repositoryError || this.wildcardError) {
      this.revealFirstInvalid(() => this.host.nativeElement);
      return;
    }
    this.step.set('access');
    if (this.usesSsh() && this.deployKey() === null) {
      this.projects.getSSHKey(this.projectName).subscribe({
        next: (key) => this.deployKey.set(key),
        error: () => this.deployKey.set(null),
      });
    }
  }

  testAccess(): void {
    if (this.testing()) return;
    this.testing.set(true);
    this.testError.set(null);
    this.repositoryAccessTestPassedForCurrentUrl.set(false);
    this.tasks.testRepositoryCloneAccess(this.projectName, this.form.repository.trim()).subscribe({
      next: () => {
        this.testing.set(false);
        this.repositoryAccessTestPassedForCurrentUrl.set(true);
      },
      error: (error: unknown) => {
        this.testing.set(false);
        this.testError.set(apiErrorMessage(error, "Couldn't test access"));
      },
    });
  }

  continueToTriggers(): void {
    this.step.set('triggers');
    this.integrations.listProjectIntegrationSummaries(this.projectName).subscribe({
      next: (list) => this.inboundIntegrations.set(list.filter((i) => i.kind === 'inbound')),
      error: () => {},
    });
    this.triggers.getSuggestedPushAndPullRequestTriggerConfigs(this.projectName, this.form.repository.trim()).subscribe({
      next: (configs) => this.suggestions.set(configs.map((config) => ({ config, selected: false }))),
      error: () => this.suggestions.set([]),
    });
  }

  suggestionLabel(config: TriggerConfig): string {
    const id = 'integration_id' in config ? config.integration_id : null;
    const integration = this.inboundIntegrations().find((i) => i.id === id);
    const from = integration ? integration.display_name || integration.name : this.form.repository.trim();
    return config.type === 'reporter_pull_request' ? `Build pull requests from ${from}` : `Build pushes from ${from}`;
  }

  toggleSuggestion(index: number, selected: boolean): void {
    this.suggestions.update((list) => list.map((choice, i) => (i === index ? { ...choice, selected } : choice)));
  }

  back(): void {
    const previous = STEPS[this.stepIndex() - 1];
    if (previous) this.step.set(previous.id);
  }

  createTaskWithTriggersAndOpenTask(): void {
    if (this.creating() || this.cronError) return;
    this.creating.set(true);
    this.createError.set(null);
    this.tasks
      .createTask(this.projectName, {
        name: this.form.name,
        display_name: this.form.display_name,
        description: '',
        repository: this.form.repository.trim(),
        wildcard: this.form.wildcard.trim() || DEFAULT_WILDCARD,
        exec_enabled: this.form.exec_enabled,
        triggers: buildTaskCreationTriggersFromWizardChoices({ ...this.form, suggestions: this.suggestions() }),
      })
      .subscribe({
        next: (task) => {
          this.creating.set(false);
          this.notify.success('Task created');
          this.router.navigateByUrl(links.task(this.projectName, task.name));
        },
        error: (error: unknown) => {
          this.creating.set(false);
          this.createError.set(apiErrorMessage(error, "Couldn't create task"));
        },
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
    return list !== null && !hasActiveCacheSubscriptionWithWriteAccess(list);
  }
}
