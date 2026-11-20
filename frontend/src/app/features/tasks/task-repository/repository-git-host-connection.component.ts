/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, inject, input, linkedSignal, output, signal, viewChild } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { RouterLink } from '@angular/router';
import { ButtonComponent, CheckboxComponent, FormFieldComponent, SelectComponent, SettingsSectionComponent } from '@gradient/ui/ui';
import { GitHostConnectionOwner, TaskRepositoryConnectionAndCommitStatusPatchRequest, TaskRepositoryResponse } from '@core/models';
import { TasksService } from '@core/services/tasks.service';
import { NotifyService } from '@core/services/notify.service';
import { repositoryHost } from '@shared/git';
import { links } from '@shared/routing';
import { GitHostConnectionDialogComponent, GitHostConnectionDialogPrefill } from '@shared/git-host-connections/git-host-connection-dialog.component';
import { gitHostConnectionPickerLabel } from '@shared/git-host-connections/git-host-connection-health';
import { nextChainAction } from '@shared/git-host-connections/task-git-host-chain.component';

@Component({
  selector: 'app-repository-git-host-connection',
  standalone: true,
  imports: [FormsModule, RouterLink, ButtonComponent, CheckboxComponent, FormFieldComponent, SelectComponent, SettingsSectionComponent, GitHostConnectionDialogComponent],
  templateUrl: './repository-git-host-connection.component.html',
  changeDetection: ChangeDetectionStrategy.OnPush,
  styles: `
    @use '@gradient/ui/styles/variables' as *;

    .connection-picker { display: flex; gap: $spacing-sm; align-items: center; }
    .connection-picker > gr-select { flex: 1; min-width: 0; }
    .report-status { display: flex; flex-wrap: wrap; gap: $spacing-md; align-items: center; }
  `,
})
export class RepositoryGitHostConnectionAndCommitStatusComponent {
  protected readonly links = links;
  private tasks = inject(TasksService);
  private notify = inject(NotifyService);
  private dialog = viewChild.required(GitHostConnectionDialogComponent);

  project = input.required<string>();
  task = input.required<string>();
  state = input.required<TaskRepositoryResponse>();
  saved = output<TaskRepositoryResponse>();
  changed = output<void>();

  selectedId = linkedSignal(() => this.state().git_host_connection?.id ?? '');
  reportStatus = linkedSignal(() => this.state().report_status);
  savingSelection = signal(false);
  savingReportStatus = signal(false);
  testingStatusReport = signal(false);

  owner = computed<GitHostConnectionOwner>(() => ({ type: 'project', name: this.project() }));
  host = computed(() => repositoryHost(this.state().repository) ?? this.state().repository);
  fittingConnections = computed(() => {
    const currentId = this.state().git_host_connection?.id;
    return this.state().available_connections.filter((connection) => connection.repository_fit?.fits !== false || connection.id === currentId);
  });
  selectFittingConnectionOrFocusPicker(): void {
    const fitting = this.state().available_connections.filter((connection) => connection.repository_fit?.fits !== false);
    if (fitting.length === 1) this.selectConnection(fitting[0].id);
    else this.focusPicker();
  }
  pickerOptions = computed(() => [
    { label: 'No connection', value: '' },
    ...this.fittingConnections().map((connection) => ({ value: connection.id, label: gitHostConnectionPickerLabel(connection) })),
  ]);
  projectConnectionNames = computed(() =>
    this.state().available_connections.filter((connection) => connection.owner.type === 'project').map((connection) => connection.name),
  );
  chainOffers = computed(() => nextChainAction(this.state().chain)?.type ?? null);
  connectionReportsStatus = computed(() => this.state().git_host_connection?.reports_status ?? false);
  connectionLink = computed(() => {
    const connection = this.state().git_host_connection;
    if (!connection || (connection.owner.type !== 'project' && !connection.can_manage)) return null;
    return links.gitHostConnection(connection.owner, connection.id);
  });

  selectConnection(id: string | null): void {
    const value = id ?? '';
    if (!this.state().can_edit || this.savingSelection() || value === (this.state().git_host_connection?.id ?? '')) return;
    this.selectedId.set(value);
    this.savingSelection.set(true);
    this.update({ git_host_connection: value || null }, () => this.savingSelection.set(false), () => this.selectedId.set(this.state().git_host_connection?.id ?? ''));
  }

  setReportStatus(reportStatus: boolean): void {
    if (!this.state().can_edit || this.savingReportStatus()) return;
    this.reportStatus.set(reportStatus);
    this.savingReportStatus.set(true);
    this.update({ report_status: reportStatus }, () => this.savingReportStatus.set(false), () => this.reportStatus.set(this.state().report_status));
  }

  openCreateConnection(prefill: GitHostConnectionDialogPrefill = { base_url: this.state().repository }): void {
    this.dialog().openCreate(prefill);
  }

  focusPicker(): void {
    document.getElementById('task-git-host-connection')?.focus();
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
        this.notify.failure("Couldn't test the status report", error);
        this.changed.emit();
      },
    });
  }

  private update(body: TaskRepositoryConnectionAndCommitStatusPatchRequest, done: () => void, revert: () => void): void {
    this.tasks.updateTaskRepositoryConnection(this.project(), this.task(), body).subscribe({
      next: (state) => {
        done();
        this.saved.emit(state);
      },
      error: (error: unknown) => {
        done();
        revert();
        this.notify.failure("Couldn't save the git host connection of the task", error);
      },
    });
  }
}
