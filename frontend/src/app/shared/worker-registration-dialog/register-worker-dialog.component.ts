/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, ElementRef, computed, effect, inject, input, model, output, signal, untracked, viewChild } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { RouterLink } from '@angular/router';
import { Observable } from 'rxjs';
import {
  ButtonComponent,
  CopyFieldComponent,
  DialogComponent,
  FormFieldComponent,
  InputDirective,
  MessageBannerComponent,
  SubmitDirective,
} from '@gradient/ui/ui';
import { ALL_ALLOWED, type AllowedCapabilities, type RegisterWorkerRequest, type WorkerRegistrationResponse } from '@core/models';
import { apiErrorMessage } from '@core/services/api.service';
import { TeamsService } from '@core/services/teams.service';
import { WorkersService } from '@core/services/workers.service';
import { dialogFormChangesSinceOpen, injectRevealFirstInvalid } from '@shared/forms';
import { checkWorkerUrlWithWebSocketOrHttpScheme } from '@shared/forms/build-worker-url-field-check';
import { links } from '@shared/routing';
import { AllowedCapabilitiesComponent, LabelHelpComponent } from '@shared/ui';

interface RegisterWorkerForm {
  display_name: string;
  url: string;
  allowed: AllowedCapabilities;
}

const emptyForm = (): RegisterWorkerForm => ({ display_name: '', url: '', allowed: ALL_ALLOWED });

@Component({
  selector: 'app-register-worker-dialog',
  standalone: true,
  imports: [
    AllowedCapabilitiesComponent,
    ButtonComponent,
    CopyFieldComponent,
    DialogComponent,
    FormFieldComponent,
    FormsModule,
    InputDirective,
    LabelHelpComponent,
    MessageBannerComponent,
    RouterLink,
    SubmitDirective,
  ],
  templateUrl: './register-worker-dialog.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class RegisterWorkerDialogComponent {
  private workers = inject(WorkersService);
  private teams = inject(TeamsService);
  private revealFirstInvalid = injectRevealFirstInvalid();
  private registerFields = viewChild<ElementRef<HTMLElement>>('registerFields');

  visible = model(false);
  project = input<string | null>(null);
  team = input<string | null>(null);
  workerRegistered = output<WorkerRegistrationResponse>();

  protected form: RegisterWorkerForm = emptyForm();
  protected formChanges = dialogFormChangesSinceOpen(() => this.form);
  protected registering = signal(false);
  protected submitAttempted = signal(false);
  protected error = signal<string | null>(null);
  workerRegistrationResponse = signal<WorkerRegistrationResponse | null>(null);

  protected workerLink = computed(() => {
    const registration = this.workerRegistrationResponse();
    const project = this.project();
    return registration && project ? links.workerMetrics(project, registration.id) : null;
  });

  constructor() {
    effect(() => {
      if (!this.visible()) return;
      untracked(() => {
        this.form = emptyForm();
        this.submitAttempted.set(false);
        this.error.set(null);
        this.formChanges.mark();
      });
    });
  }

  protected displayNameError(): string | null {
    return this.submitAttempted() && !this.form.display_name.trim() ? 'Enter a display name.' : null;
  }

  protected urlError(): string | null {
    return checkWorkerUrlWithWebSocketOrHttpScheme(this.form.url);
  }

  protected register(): void {
    if (this.registering()) return;
    this.submitAttempted.set(true);
    if (this.displayNameError() || this.urlError()) {
      this.revealFirstInvalid(() => this.registerFields()?.nativeElement);
      return;
    }
    const worker: RegisterWorkerRequest = {
      display_name: this.form.display_name.trim(),
      url: this.form.url.trim() || undefined,
      ...this.form.allowed,
    };
    this.registering.set(true);
    this.error.set(null);
    this.request(worker).subscribe({
      next: (registration) => {
        this.registering.set(false);
        this.visible.set(false);
        this.workerRegistrationResponse.set(registration);
        this.workerRegistered.emit(registration);
      },
      error: (err: unknown) => {
        this.registering.set(false);
        this.error.set(apiErrorMessage(err, "Couldn't register the worker."));
      },
    });
  }

  private request(worker: RegisterWorkerRequest): Observable<WorkerRegistrationResponse> {
    const project = this.project();
    return project ? this.workers.registerWorker(project, worker) : this.teams.registerWorker(this.team() ?? '', worker);
  }

  protected closeResult(): void {
    this.workerRegistrationResponse.set(null);
  }
}
