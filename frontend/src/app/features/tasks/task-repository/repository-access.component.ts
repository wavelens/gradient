/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, inject, input, output, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import {
  ButtonComponent,
  CopyFieldComponent,
  FormFieldComponent,
  InputDirective,
  SettingsSectionComponent,
} from '@gradient/ui/ui';
import { TaskRepositoryResponse } from '@core/models';
import { TasksService } from '@core/services/tasks.service';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import { RepositoryFetchBannerComponent, gitHostSettingsLinks } from '@shared/git';
import { repositoryUrlError } from '@shared/text/repository-url';
import { relativeTime } from '@shared/text';
import { fieldChecks, firstFieldError } from '@shared/forms';

@Component({
  selector: 'app-repository-access',
  standalone: true,
  imports: [
    FormsModule,
    ButtonComponent,
    CopyFieldComponent,
    FormFieldComponent,
    InputDirective,
    SettingsSectionComponent,
    RepositoryFetchBannerComponent,
  ],
  templateUrl: './repository-access.component.html',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class RepositoryAccessComponent {
  private tasks = inject(TasksService);
  private notify = inject(NotifyService);

  project = input.required<string>();
  task = input.required<string>();
  state = input.required<TaskRepositoryResponse>();
  gitHost = input<string | null>(null);
  changed = output<void>();

  editing = signal(false);
  draftUrl = signal('');
  saving = signal(false);
  testing = signal(false);
  testError = signal<string | null>(null);

  urlError = computed(() => {
    const url = this.draftUrl().trim();
    if (!url) return 'Enter the repository URL.';
    return firstFieldError(url, fieldChecks.remoteRepository, (value) => repositoryUrlError(String(value)));
  });
  links = computed(() => gitHostSettingsLinks(this.state().repository, this.gitHost()));
  fetchOk = computed(() => {
    const fetch = this.state().fetch;
    return fetch && !fetch.failed ? fetch : null;
  });

  startEdit(): void {
    this.draftUrl.set(this.state().repository);
    this.editing.set(true);
  }

  saveUrl(): void {
    if (this.urlError() || this.saving()) return;
    this.saving.set(true);
    this.tasks.updateTask(this.project(), this.task(), { repository: this.draftUrl().trim() }).subscribe({
      next: () => {
        this.saving.set(false);
        this.editing.set(false);
        this.notify.success('Repository URL saved');
        this.changed.emit();
      },
      error: (error: unknown) => {
        this.saving.set(false);
        this.notify.failure('Failed to save repository URL', error);
      },
    });
  }

  testAccess(): void {
    this.testing.set(true);
    this.testError.set(null);
    this.tasks.checkRepository(this.project(), this.task(), this.state().repository).subscribe({
      next: () => {
        this.testing.set(false);
        this.changed.emit();
      },
      error: (error: unknown) => {
        this.testing.set(false);
        this.testError.set(apiErrorMessage(error, 'Failed to test access'));
        this.changed.emit();
      },
    });
  }

  since(at: string | null | undefined): string {
    return relativeTime(at);
  }
}
