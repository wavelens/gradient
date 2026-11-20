/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, inject, input, linkedSignal, output, signal, viewChild } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { RouterLink } from '@angular/router';
import { ButtonComponent, SelectComponent, SettingsSectionComponent } from '@gradient/ui/ui';
import { GitHostConnectionOwner, TaskGitHostChainStepAction, TaskRepositoryResponse } from '@core/models';
import { TasksService } from '@core/services/tasks.service';
import { NotifyService } from '@core/services/notify.service';
import { links } from '@shared/routing';
import { GitHostConnectionDialogComponent } from '@shared/git-host-connections/git-host-connection-dialog.component';
import { gitHostConnectionPickerOptions } from '@shared/git-host-connections/git-host-connection-health';
import { TaskRepositoryStatusCheckStepStateBadgeComponent } from '@shared/git-host-connections/task-repository-status-check-step-state-badge.component';
import { GithubAppInstallLinkComponent } from '@shared/git-host-connections/github-app-install-link.component';

@Component({
  selector: 'app-repository-git-host-connection',
  standalone: true,
  imports: [FormsModule, RouterLink, ButtonComponent, TaskRepositoryStatusCheckStepStateBadgeComponent, SelectComponent, SettingsSectionComponent, GitHostConnectionDialogComponent, GithubAppInstallLinkComponent],
  templateUrl: './repository-git-host-connection.component.html',
  changeDetection: ChangeDetectionStrategy.OnPush,
  styles: `
    @use '@gradient/ui/styles/variables' as *;

    .connection-picker { display: flex; flex-wrap: wrap; gap: $spacing-sm; align-items: center; }
    .connection-picker > gr-select { flex: 1 1 12rem; min-width: 0; }
    .no-connections { display: flex; flex-wrap: wrap; align-items: center; gap: $spacing-sm; margin: 0; overflow-wrap: anywhere; }
  `,
})
export class RepositoryGitHostConnectionComponent {
  protected readonly links = links;
  private tasks = inject(TasksService);
  private notify = inject(NotifyService);
  private dialog = viewChild.required(GitHostConnectionDialogComponent);

  project = input.required<string>();
  task = input.required<string>();
  state = input.required<TaskRepositoryResponse>();
  hasPrimaryAction = input(false);
  saved = output<TaskRepositoryResponse>();
  changed = output<void>();

  selectedId = linkedSignal(() => this.state().git_host_connection?.id ?? '');
  savingSelection = signal(false);

  step = computed(() => this.state().chain.connection);
  owner = computed<GitHostConnectionOwner>(() => ({ type: 'project', name: this.project() }));
  onlyFittingConnection = computed(() => {
    if (this.step().action?.type !== 'select_connection') return null;
    const fitting = this.state().available_connections.filter((connection) => connection.repository_fit?.fits !== false);
    return fitting.length === 1 ? fitting[0] : null;
  });
  pickerOptions = computed(() => gitHostConnectionPickerOptions(this.state().available_connections, this.state().git_host_connection?.id));
  projectConnectionNames = computed(() =>
    this.state().available_connections.filter((connection) => connection.owner.type === 'project').map((connection) => connection.name),
  );
  connectionLink = computed(() => {
    const connection = this.state().git_host_connection;
    if (!connection || (connection.owner.type !== 'project' && !connection.can_manage)) return null;
    return links.gitHostConnection(connection.owner, connection.id);
  });

  isPrimary(type: TaskGitHostChainStepAction['type']): boolean {
    return this.hasPrimaryAction() && this.step().action?.type === type;
  }

  selectConnection(id: string | null): void {
    const value = id ?? '';
    if (!this.state().can_edit || this.savingSelection() || value === (this.state().git_host_connection?.id ?? '')) return;
    this.selectedId.set(value);
    this.savingSelection.set(true);
    this.tasks.updateTaskRepositoryConnection(this.project(), this.task(), { git_host_connection: value || null }).subscribe({
      next: (state) => {
        this.savingSelection.set(false);
        this.saved.emit(state);
      },
      error: (error: unknown) => {
        this.savingSelection.set(false);
        this.selectedId.set(this.state().git_host_connection?.id ?? '');
        this.notify.failure('Failed to save the connection to the Git host for the task', error);
      },
    });
  }

  openCreateConnection(): void {
    const action = this.step().action;
    this.dialog().openCreate(
      action?.type === 'create_connection' ? { git_host_type: action.git_host_type, base_url: action.base_url } : { base_url: this.state().repository },
    );
  }
}
