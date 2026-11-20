/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, inject, input, linkedSignal, output, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { ButtonComponent, CheckboxComponent, SettingsSectionComponent } from '@gradient/ui/ui';
import { TaskRepositoryResponse, eventNameToReadableText } from '@core/models';
import { TasksService } from '@core/services/tasks.service';
import { NotifyService } from '@core/services/notify.service';
import { RelativeTimePipe, ServerTimestampToLocalDateTimePipe } from '@shared/text';
import { TaskRepositoryStatusCheckStepStateBadgeComponent } from '@shared/git-host-connections/task-repository-status-check-step-state-badge.component';
import { statusPostSummary } from '@shared/git-host-connections/git-host-connection-health';
import { StepHistoryLine, StepHistoryListComponent } from './step-history-list.component';

const EARLIER_POSTS_SHOWN = 4;

@Component({
  selector: 'app-repository-commit-status',
  standalone: true,
  imports: [FormsModule, ButtonComponent, CheckboxComponent, TaskRepositoryStatusCheckStepStateBadgeComponent, RelativeTimePipe, ServerTimestampToLocalDateTimePipe, SettingsSectionComponent, StepHistoryListComponent],
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <gr-settings-section title="Commit Status">
      <app-task-repository-status-check-step-state-badge slot="after-title" [state]="step().state" />
      @if (!applies()) {
        <span class="text-secondary">{{ step().detail }}</span>
      } @else {
        <div class="report-status">
          <gr-checkbox inputId="task-report-commit-status" label="Report commit status" [ngModel]="reportStatus()"
            (ngModelChange)="setReportStatus($event)" [disabled]="!state().can_edit || savingReportStatus()" />
          @if (state().can_edit && state().report_status) {
            <button grButton size="small" label="Test the status report" icon="send" [severity]="testIsPrimary() ? 'primary' : 'secondary'"
              (click)="testStatusReport()" [loading]="testingStatusReport()" [disabled]="testingStatusReport()" data-testid="test-status-report"></button>
          }
        </div>
        @if (state().report_status) {
          <p class="step-status" data-testid="step-status">
            <span>{{ step().detail }}</span>
            @if (step().at; as at) {
              <span class="text-secondary" [title]="at | serverDate">{{ at | relativeTime }}</span>
            }
          </p>
          @if (earlierPosts().length > 0) {
            <app-step-history-list [lines]="earlierPosts()" />
          }
        }
      }
    </gr-settings-section>
  `,
  styles: `
    @use '@gradient/ui/styles/variables' as *;

    .report-status { display: flex; flex-wrap: wrap; gap: $spacing-md; align-items: center; }
    .step-status { display: flex; flex-wrap: wrap; gap: $spacing-sm; margin: $spacing-md 0 0; overflow-wrap: anywhere; }
  `,
})
export class RepositoryCommitStatusComponent {
  private tasks = inject(TasksService);
  private notify = inject(NotifyService);

  project = input.required<string>();
  task = input.required<string>();
  state = input.required<TaskRepositoryResponse>();
  hasPrimaryAction = input(false);
  saved = output<TaskRepositoryResponse>();
  changed = output<void>();

  reportStatus = linkedSignal(() => this.state().report_status);
  savingReportStatus = signal(false);
  testingStatusReport = signal(false);

  step = computed(() => this.state().chain.commit_status);
  applies = computed(() => this.state().chain.connection.state === 'ok' && !!this.state().git_host_connection?.reports_status);
  testIsPrimary = computed(() => this.hasPrimaryAction() && this.step().action?.type === 'test_status_report');
  earlierPosts = computed<StepHistoryLine[]>(() => {
    const posts = this.state().status_posts;
    const earlier = posts[0]?.delivered_at === this.step().at ? posts.slice(1) : posts;
    return earlier.slice(0, EARLIER_POSTS_SHOWN).map((post) => ({
      id: post.id,
      label: eventNameToReadableText(post.event),
      severity: post.success ? 'success' : 'danger',
      text: statusPostSummary(post),
      at: post.delivered_at,
    }));
  });

  setReportStatus(reportStatus: boolean): void {
    if (!this.state().can_edit || this.savingReportStatus()) return;
    this.reportStatus.set(reportStatus);
    this.savingReportStatus.set(true);
    this.tasks.updateTaskRepositoryConnection(this.project(), this.task(), { report_status: reportStatus }).subscribe({
      next: (state) => {
        this.savingReportStatus.set(false);
        this.saved.emit(state);
      },
      error: (error: unknown) => {
        this.savingReportStatus.set(false);
        this.reportStatus.set(this.state().report_status);
        this.notify.failure('Failed to save commit status setting for task', error);
      },
    });
  }

  testStatusReport(): void {
    if (this.testingStatusReport()) return;
    this.testingStatusReport.set(true);
    this.tasks.postPendingTestCommitStatus(this.project(), this.task()).subscribe({
      next: (attempt) => {
        this.testingStatusReport.set(false);
        if (attempt.success) this.notify.success('Status report sent');
        else this.notify.failure(`Status report failed: ${attempt.error_message ?? `HTTP ${attempt.response_status ?? 'error'}`}`, null);
        this.changed.emit();
      },
      error: (error: unknown) => {
        this.testingStatusReport.set(false);
        this.notify.failure('Status report test failed', error);
        this.changed.emit();
      },
    });
  }
}
