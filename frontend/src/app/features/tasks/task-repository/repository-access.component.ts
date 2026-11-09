/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, inject, input, output, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Observable, of, switchMap, tap } from 'rxjs';
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
import { ApiError, apiErrorMessage } from '@core/services/api.service';
import { RepositoryFetchBannerComponent, fetchFailureHint, gitHostSettingsLinks, repositoryHost } from '@shared/git';
import { ConfirmService } from '@shared/confirm';
import { repositoryUrlError } from '@shared/text/repository-url';
import { RelativeTimePipe } from '@shared/text';
import { fieldChecks, firstFieldError, unsavedChanges } from '@shared/forms';
import { provisioningIntegration } from './provisioning';

@Component({
  selector: 'app-repository-access',
  standalone: true,
  imports: [
    RelativeTimePipe,
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
  private confirm = inject(ConfirmService);

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
  unsavedRepositoryUrlAccessTestPassed = signal(false);
  installingKey = signal(false);
  readonly unsaved = unsavedChanges(() => this.editing() && this.draftUrl().trim() !== this.state().repository, 'Repository');

  urlError = computed(() => {
    const url = this.draftUrl().trim();
    if (!url) return 'Enter the repository URL.';
    return firstFieldError(url, fieldChecks.remoteRepository, (value) => repositoryUrlError(String(value)));
  });
  links = computed(() => gitHostSettingsLinks(this.state().repository, this.gitHost()));
  isGitHub = computed(() => repositoryHost(this.state().repository) === 'github.com');
  provisioner = computed(() => (this.state().deploy_key ? provisioningIntegration(this.state()) : null));
  fetchOk = computed(() => {
    const fetch = this.state().fetch;
    return fetch && !fetch.failed ? fetch : null;
  });

  startEdit(): void {
    this.draftUrl.set(this.state().repository);
    this.editing.set(true);
    this.testError.set(null);
    this.unsavedRepositoryUrlAccessTestPassed.set(false);
    this.unsaved.mark();
  }

  cancelEdit(): void {
    this.editing.set(false);
    this.testError.set(null);
    this.unsavedRepositoryUrlAccessTestPassed.set(false);
  }

  saveUrl(): void {
    if (this.urlError() || this.saving() || this.testing()) return;
    const url = this.draftUrl().trim();
    this.saving.set(true);
    this.testError.set(null);
    const check: Observable<unknown> = this.unsavedRepositoryUrlAccessTestPassed()
      ? of(null)
      : this.tasks.checkRepository(this.project(), this.task(), url);
    check.pipe(switchMap(() => this.saveRepositoryUrlAndRunAfterSaveSteps(url))).subscribe({
      next: () => this.saving.set(false),
      error: (error: unknown) => {
        this.saving.set(false);
        if (error instanceof ApiError && error.code === 'repository_unreachable') {
          this.confirmSaveRepositoryUrlAfterFailedFetch(url, error.message);
        } else {
          this.notify.failure("Couldn't save repository URL", error);
        }
      },
    });
  }

  private confirmSaveRepositoryUrlAfterFailedFetch(url: string, reason: string): void {
    this.confirm.confirm({
      title: 'Save unreachable URL',
      target: url,
      consequence: ["Gradient can't access this URL. Evaluations fail until Gradient can access the repository.", reason, fetchFailureHint(reason)]
        .filter(Boolean)
        .join(' '),
      confirmLabel: 'Save as is',
      failure: "Couldn't save repository URL",
      action: () => this.saveRepositoryUrlAndRunAfterSaveSteps(url),
    }).subscribe();
  }

  private saveRepositoryUrlAndRunAfterSaveSteps(url: string): Observable<string> {
    return this.tasks.updateTask(this.project(), this.task(), { repository: url }).pipe(
      tap(() => {
        this.cancelEdit();
        this.notify.success('Repository URL saved');
        this.changed.emit();
        this.checkSavedUrl(url);
      }),
    );
  }

  testAccess(): void {
    if (!this.editing()) {
      this.checkSavedUrl(this.state().repository);
      return;
    }
    if (this.urlError()) return;
    this.startCheck();
    this.tasks.checkRepository(this.project(), this.task(), this.draftUrl().trim()).subscribe({
      next: () => {
        this.testing.set(false);
        this.unsavedRepositoryUrlAccessTestPassed.set(true);
      },
      error: (error: unknown) => {
        this.testing.set(false);
        this.testError.set(apiErrorMessage(error, "Couldn't test access"));
      },
    });
  }

  private checkSavedUrl(url: string): void {
    this.startCheck();
    this.tasks.checkRepository(this.project(), this.task(), url).subscribe({
      next: () => {
        this.testing.set(false);
        this.changed.emit();
      },
      error: (error: unknown) => {
        this.testing.set(false);
        if (!(error instanceof ApiError && error.code === 'repository_unreachable')) {
          this.testError.set(apiErrorMessage(error, "Couldn't test access"));
        }
        this.changed.emit();
      },
    });
  }

  private startCheck(): void {
    this.testing.set(true);
    this.testError.set(null);
    this.unsavedRepositoryUrlAccessTestPassed.set(false);
  }

  installDeployKey(): void {
    const outbound = this.provisioner()?.id;
    if (!outbound || this.installingKey()) return;
    this.installingKey.set(true);
    this.tasks.installDeployKey(this.project(), this.task(), { outbound_integration: outbound }).subscribe({
      next: () => {
        this.installingKey.set(false);
        this.notify.success('Deploy key installed');
        this.testAccess();
      },
      error: (error: unknown) => {
        this.installingKey.set(false);
        this.notify.failure("Couldn't install deploy key", error);
      },
    });
  }
}
