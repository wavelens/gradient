/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, input, linkedSignal, output, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { RouterLink } from '@angular/router';
import {
  BadgeComponent,
  ButtonComponent,
  CheckboxComponent,
  CopyFieldComponent,
  FormFieldComponent,
  SelectComponent,
  SettingsSectionComponent,
  SubmitDirective,
} from '@gradient/ui/ui';
import {
  TaskCommitStatusPostAttempt,
  TaskRepositoryResponse,
  eventNameToReadableText,
  getGitHostConnectionOwnerReadableText,
} from '@core/models';
import { TasksService } from '@core/services/tasks.service';
import { NotifyService } from '@core/services/notify.service';
import { ConfigService } from '@core/services/config.service';
import { AuthService } from '@core/services/auth.service';
import { deliverySummary, gitHostLabel, gitHostSettingsLinks, repositoryHost, webhookEvents } from '@shared/git';
import { CurrentTimeSignal, RelativeTimePipe, relativeTime } from '@shared/text';
import { links } from '@shared/routing';
import { unsavedChanges } from '@shared/forms';
import { canInstallWebhookOrDeployKeyForTask } from './provisioning';

@Component({
  selector: 'app-repository-git-host-connection',
  standalone: true,
  imports: [
    FormsModule,
    RouterLink,
    BadgeComponent,
    ButtonComponent,
    CheckboxComponent,
    CopyFieldComponent,
    FormFieldComponent,
    RelativeTimePipe,
    SelectComponent,
    SettingsSectionComponent,
    SubmitDirective,
  ],
  templateUrl: './repository-git-host-connection.component.html',
  changeDetection: ChangeDetectionStrategy.OnPush,
  styles: `
    @use '@gradient/ui/styles/variables' as *;

    .status-posts { list-style: none; margin: 0; padding: 0; }
    .status-posts li { display: flex; align-items: baseline; gap: $spacing-sm; padding: $spacing-xs 0; }
    .status-post-error { flex: 1; word-break: break-word; }
  `,
})
export class RepositoryGitHostConnectionAndCommitStatusComponent implements OnInit {
  protected readonly links = links;
  protected readonly eventNameToReadableText = eventNameToReadableText;
  private tasks = inject(TasksService);
  private notify = inject(NotifyService);
  private config = inject(ConfigService);
  private auth = inject(AuthService);
  private clock = inject(CurrentTimeSignal);

  project = input.required<string>();
  task = input.required<string>();
  state = input.required<TaskRepositoryResponse>();
  taskRepositoryResponseAfterGitHostSave = output<TaskRepositoryResponse>();
  changed = output<void>();

  selectedGitHostConnectionId = linkedSignal(() => this.state().git_host_connection?.id ?? '');
  reportCommitStatusChecked = linkedSignal(() => this.state().report_status);
  saving = signal(false);
  isStatusReportTestRunning = signal(false);
  testStatusReportResult = signal<TaskCommitStatusPostAttempt | null>(null);

  gitHostConnectionDropdownOptions = computed(() => [
    { label: 'No connection', value: '' },
    ...this.state().available_connections.map((connection) => ({
      value: connection.id,
      label: [
        connection.display_name,
        getGitHostConnectionOwnerReadableText(connection.owner),
        gitHostLabel(connection.git_host_type),
        ...(connection.receives_events ? ['events'] : []),
        ...(connection.reports_status ? ['status'] : []),
      ].join(' · '),
    })),
  ]);
  selectedGitHostConnectionSummary = computed(
    () => this.state().available_connections.find((connection) => connection.id === this.selectedGitHostConnectionId()) ?? null,
  );
  selectedConnectionCanReportCommitStatus = computed(() => this.selectedGitHostConnectionSummary()?.reports_status ?? false);
  gitHostConnectionFormDiffersFromSaved = computed(
    () =>
      this.selectedGitHostConnectionId() !== (this.state().git_host_connection?.id ?? '') ||
      this.reportCommitStatusChecked() !== this.state().report_status,
  );
  readonly unsaved = unsavedChanges(() => this.gitHostConnectionFormDiffersFromSaved(), 'Git host connection');
  testStatusReportButtonDisabledReason = computed(() => {
    if (this.gitHostConnectionFormDiffersFromSaved()) return 'Save the connection before the test.';
    if (!this.state().report_status) return 'Turn on reporting of commit status and save first.';
    return null;
  });

  isGitHub = computed(() => repositoryHost(this.state().repository) === 'github.com');
  availableGitHostConnectionsIncludeGitHubAppConnection = computed(() => this.state().available_connections.some((connection) => connection.git_host_type === 'github'));
  gitHubConnectionHintForRepositoryWithoutConnection = computed(() => {
    if (this.config.githubAppEnabled) return 'install';
    return this.auth.user()?.superuser ? 'admin' : 'ask';
  });

  savedGitHostConnectionReceivesEvents = computed(() => this.state().git_host_connection?.receives_events ?? false);
  showInstallWebhookButton = computed(() => canInstallWebhookOrDeployKeyForTask(this.state()) && this.savedGitHostConnectionReceivesEvents());
  hostLinks = computed(() => gitHostSettingsLinks(this.state().repository, this.state().git_host_connection?.git_host_type ?? null));
  events = computed(() => webhookEvents(this.state().git_host_connection?.git_host_type ?? 'forgejo'));
  lastEvent = computed(() => {
    const event = this.state().last_event;
    return event ? `${deliverySummary(event)} · ${relativeTime(event.created_at, this.clock.currentTimeMilliseconds())}` : null;
  });
  installingWebhook = signal(false);

  ngOnInit(): void {
    this.unsaved.mark();
  }

  selectGitHostConnectionAndUpdateReportCommitStatus(id: string): void {
    this.selectedGitHostConnectionId.set(id ?? '');
    if (!this.selectedConnectionCanReportCommitStatus()) this.reportCommitStatusChecked.set(false);
  }

  save(): void {
    if (!this.state().can_edit || !this.gitHostConnectionFormDiffersFromSaved() || this.saving()) return;
    this.saving.set(true);
    this.tasks
      .updateTaskRepositoryConnection(this.project(), this.task(), {
        git_host_connection: this.selectedGitHostConnectionId() || null,
        report_status: this.reportCommitStatusChecked(),
      })
      .subscribe({
        next: (state) => {
          this.saving.set(false);
          this.testStatusReportResult.set(null);
          this.notify.success('Repository connection saved');
          this.taskRepositoryResponseAfterGitHostSave.emit(state);
        },
        error: (error: unknown) => {
          this.saving.set(false);
          this.notify.failure("Couldn't save repository connection", error);
        },
      });
  }

  postPendingTestCommitStatusAndShowResultOnTestStatusReportButtonClick(): void {
    if (this.testStatusReportButtonDisabledReason() || this.isStatusReportTestRunning()) return;
    this.isStatusReportTestRunning.set(true);
    this.tasks.postPendingTestCommitStatus(this.project(), this.task()).subscribe({
      next: (attempt) => {
        this.isStatusReportTestRunning.set(false);
        this.testStatusReportResult.set(attempt);
        this.changed.emit();
      },
      error: (error: unknown) => {
        this.isStatusReportTestRunning.set(false);
        this.notify.failure('Could not test the status report', error);
        this.changed.emit();
      },
    });
  }

  installWebhook(): void {
    if (!this.showInstallWebhookButton() || this.installingWebhook()) return;
    this.installingWebhook.set(true);
    this.tasks.installWebhook(this.project(), this.task()).subscribe({
      next: () => {
        this.installingWebhook.set(false);
        this.notify.success('Webhook installed');
      },
      error: (error: unknown) => {
        this.installingWebhook.set(false);
        this.notify.failure("Couldn't install webhook", error);
      },
    });
  }
}
