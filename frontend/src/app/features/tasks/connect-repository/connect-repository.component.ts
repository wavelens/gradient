/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, ElementRef, OnInit, computed, inject, signal, viewChild } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Subscription } from 'rxjs';
import { ActivatedRoute, Router, RouterLink } from '@angular/router';
import { links } from '@shared/routing';
import { createMissingPermissionLoadStateError, isViewerProjectMember } from '@shared/access';
import { accessTo } from '@core/models/access.model';
import { injectProject } from '@core/resolvers/project-access.resolver';
import {
  ButtonComponent,
  CheckboxComponent,
  CopyFieldComponent,
  DividerComponent,
  FormFieldComponent,
  InputDirective,
  LoadStateComponent,
  MessageBannerComponent,
  PageLayoutComponent,
  SelectComponent,
  SettingsSectionComponent,
  SubmitDirective,
} from '@gradient/ui/ui';
import { GitHostConnectionOwner, GitHostConnectionSummary, TaskRepositoryResponse, TriggerConfig } from '@core/models';
import { GitHostConnectionsApi } from '@core/services/git-host-connections.service';
import { TasksService } from '@core/services/tasks.service';
import { ProjectsService } from '@core/services/projects.service';
import { TriggersService } from '@core/services/triggers.service';
import { ConfigService } from '@core/services/config.service';
import { AuthService } from '@core/services/auth.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import { NameFieldsComponent, fieldChecks, firstFieldError, injectRevealFirstInvalid, nameAvailability } from '@shared/forms';
import { wildcardError } from '../wildcard-error';
import { LabelHelpComponent, ProjectReadinessCardComponent } from '@shared/ui';
import { repositoryUrlError } from '@shared/text/repository-url';
import { slugify } from '@shared/text';
import { repositoryWebUrl } from '@shared/evaluation';
import { repositoryHost } from '@shared/git';
import { GitHostConnectionDialogComponent } from '@shared/git-host-connections/git-host-connection-dialog.component';
import { gitHostConnectionPickerOptions } from '@shared/git-host-connections/git-host-connection-health';
import { TaskRepositoryStatusCheckSummaryComponent } from '@shared/git-host-connections/task-repository-status-check-summary.component';
import { GithubAppInstallLinkComponent } from '@shared/git-host-connections/github-app-install-link.component';
import {
  ConnectRepositoryWizardStep,
  readProjectConnectRepositoryDraftFromSessionStorage,
  removeProjectWizardDraftFromSessionStorage,
  saveProjectWizardDraftToSessionStorage,
} from '@shared/git-host-connections/connect-repository-wizard-unfinished-input-session-storage';

export const RESERVED_TASK_NAMES = ['build-request'];
const DEFAULT_WILDCARD = 'packages.x86_64-linux.**';
const DEFAULT_POLLING_INTERVAL_SECS = 300;

const STEPS: { id: ConnectRepositoryWizardStep; label: string }[] = [
  { id: 'repository', label: 'Repository' },
  { id: 'git-host', label: 'Git host' },
  { id: 'access', label: 'Access' },
  { id: 'triggers', label: 'Triggers and settings' },
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
    if (!selected || (config.type !== 'reporter_push' && config.type !== 'reporter_pull_request')) continue;
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
    ProjectReadinessCardComponent,
    RouterLink,
    ButtonComponent,
    CheckboxComponent,
    CopyFieldComponent,
    DividerComponent,
    FormFieldComponent,
    InputDirective,
    LoadStateComponent,
    MessageBannerComponent,
    PageLayoutComponent,
    SettingsSectionComponent,
    SubmitDirective,
    NameFieldsComponent,
    LabelHelpComponent,
    SelectComponent,
    GitHostConnectionDialogComponent,
    TaskRepositoryStatusCheckSummaryComponent,
    GithubAppInstallLinkComponent,
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
  private crumbs = inject(BreadcrumbsService);
  private notify = inject(NotifyService);
  private gitHostConnections = inject(GitHostConnectionsApi);
  private connectionDialog = viewChild(GitHostConnectionDialogComponent);
  private host = inject<ElementRef<HTMLElement>>(ElementRef);
  private revealFirstInvalid = injectRevealFirstInvalid();
  protected project = injectProject();
  protected readonly config = inject(ConfigService);
  private auth = inject(AuthService);
  protected isInstanceSuperuser = computed(() => !!this.auth.user()?.superuser);
  protected canInstallGithubApp = computed(() => this.isInstanceSuperuser() && !!this.config.githubAppInstallUrl);

  readonly steps = STEPS;
  access = computed(() => accessTo(this.project(), 'createTask'));
  canManageIntegrations = computed(() => accessTo(this.project(), 'manageIntegrations').canEdit);
  missingPermission = computed(() => createMissingPermissionLoadStateError('project', isViewerProjectMember(this.project()), 'createTask'));
  projectName = '';
  step = signal<ConnectRepositoryWizardStep>('repository');
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

  deployKey = signal<string | null>(null);
  testing = signal(false);
  repositoryAccessTestPassedForCurrentUrl = signal(false);
  testError = signal<string | null>(null);
  suggestions = signal<SuggestedTriggerChoice[]>([]);

  connectionOwner = computed<GitHostConnectionOwner>(() => ({ type: 'project', name: this.projectName }));
  connections = signal<GitHostConnectionSummary[] | null>(null);
  connectionsError = signal<unknown>(null);
  private connectionsRequest?: Subscription;
  selectedConnectionId = signal<string | null>(null);
  fittingConnections = computed(() => (this.connections() ?? []).filter((connection) => connection.repository_fit?.fits));
  connectionOptions = computed(() => gitHostConnectionPickerOptions(this.connections() ?? []));
  projectConnectionNames = computed(() =>
    (this.connections() ?? []).filter((connection) => connection.owner.type === 'project').map((connection) => connection.name),
  );
  created = signal<{ name: string; repository: TaskRepositoryResponse | null } | null>(null);

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
    if (this.access().canEdit) this.loadSavedWizardInputAndReopenSavedStep();
  }

  saveWizardInputAndStepToSessionStorage(): void {
    saveProjectWizardDraftToSessionStorage({
      project: this.projectName,
      step: this.step(),
      form: { ...this.form },
      selectedConnectionId: this.selectedConnectionId(),
      savedAt: Date.now(),
    });
  }

  clearSavedWizardInput(): void {
    removeProjectWizardDraftFromSessionStorage(this.projectName);
  }

  private loadSavedWizardInputAndReopenSavedStep(): void {
    const saved = readProjectConnectRepositoryDraftFromSessionStorage(this.projectName);
    if (!saved) return;
    this.form = { ...saved.form };
    this.autofilledDisplayName = repositoryName(saved.form.repository) === saved.form.display_name ? saved.form.display_name : '';
    const savedStepIndex = STEPS.findIndex((s) => s.id === saved.step);
    if (savedStepIndex >= 1) this.loadConnections(saved.selectedConnectionId);
    if (savedStepIndex >= 2) this.loadProjectSshDeployKeyIfRepositoryUrlUsesSshAndNoKeyLoaded();
    if (savedStepIndex >= 3) this.fetchSuggestedPushAndPullRequestTriggers();
    this.step.set(saved.step);
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
    this.step.set('git-host');
    this.saveWizardInputAndStepToSessionStorage();
    this.loadConnections();
  }

  repositoryHostName(): string {
    return repositoryHost(this.form.repository) ?? this.form.repository.trim();
  }

  gitHubAccount(): string | null {
    if (this.repositoryHostName() !== 'github.com') return null;
    const web = repositoryWebUrl(this.form.repository.trim());
    return web ? (new URL(web).pathname.split('/')[1] ?? null) : null;
  }

  loadConnections(selectId: string | null = null): void {
    this.connections.set(null);
    this.connectionsError.set(null);
    this.connectionsRequest?.unsubscribe();
    this.connectionsRequest = this.gitHostConnections.list(this.connectionOwner(), this.form.repository.trim()).subscribe({
      next: (list) => {
        this.connections.set(list);
        const fitting = this.fittingConnections();
        this.selectedConnectionId.set(selectId ?? (fitting.length === 1 ? fitting[0].id : null));
      },
      error: (error: unknown) => this.connectionsError.set(error),
    });
  }

  openCreateConnection(): void {
    this.connectionDialog()?.openCreate({ base_url: this.form.repository.trim() });
  }

  continueFromGitHost(): void {
    this.step.set('access');
    this.saveWizardInputAndStepToSessionStorage();
    this.loadProjectSshDeployKeyIfRepositoryUrlUsesSshAndNoKeyLoaded();
  }

  private loadProjectSshDeployKeyIfRepositoryUrlUsesSshAndNoKeyLoaded(): void {
    if (!this.usesSsh() || this.deployKey() !== null) return;
    this.projects.getSSHKey(this.projectName).subscribe({
      next: (key) => this.deployKey.set(key),
      error: () => this.deployKey.set(null),
    });
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
    this.saveWizardInputAndStepToSessionStorage();
    this.fetchSuggestedPushAndPullRequestTriggers();
  }

  private fetchSuggestedPushAndPullRequestTriggers(): void {
    this.triggers.getSuggestedPushAndPullRequestTriggerConfigs(this.projectName, this.form.repository.trim()).subscribe({
      next: (configs) => this.suggestions.set(configs.map((config) => ({ config, selected: false }))),
      error: () => this.suggestions.set([]),
    });
  }

  suggestionLabel(config: TriggerConfig): string {
    const repository = this.form.repository.trim();
    return config.type === 'reporter_pull_request' ? `Build pull requests from ${repository}` : `Build pushes from ${repository}`;
  }

  toggleSuggestion(index: number, selected: boolean): void {
    this.suggestions.update((list) => list.map((choice, i) => (i === index ? { ...choice, selected } : choice)));
  }

  back(): void {
    const previous = STEPS[this.stepIndex() - 1];
    if (!previous) return;
    this.step.set(previous.id);
    this.saveWizardInputAndStepToSessionStorage();
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
        ...(this.selectedConnectionId() === null ? {} : { git_host_connection: this.selectedConnectionId() || null }),
      })
      .subscribe({
        next: (task) => {
          this.creating.set(false);
          this.clearSavedWizardInput();
          this.notify.success('Task created');
          this.created.set({ name: task.name, repository: null });
          this.tasks.getRepository(this.projectName, task.name).subscribe({
            next: (repository) => this.created.set({ name: task.name, repository }),
            error: () => this.router.navigateByUrl(links.task(this.projectName, task.name)),
          });
        },
        error: (error: unknown) => {
          this.creating.set(false);
          this.createError.set(apiErrorMessage(error, "Couldn't create task"));
        },
      });
  }
}
