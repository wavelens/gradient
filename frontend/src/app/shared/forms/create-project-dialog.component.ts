/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, ElementRef, effect, inject, input, model, output, signal, untracked, viewChild } from '@angular/core';
import { FormsModule } from '@angular/forms';
import {
  ButtonComponent,
  DialogComponent,
  FormFieldComponent,
  InputDirective,
  SelectComponent,
  SubmitDirective,
} from '@gradient/ui/ui';
import { ProjectsService } from '@core/services/projects.service';
import { apiErrorMessage } from '@core/services/api.service';
import { NotifyService } from '@core/services/notify.service';
import type { MakeProjectRequest, TeamNameAndDisplayName } from '@core/models';
import { nameAvailability } from './name-availability';
import { NameFieldsComponent } from './name-fields.component';
import { OwnerTeamSelectComponent } from './owner-team-select.component';
import { dialogFormChangesSinceOpen } from './unsaved-changes';
import { injectRevealFirstInvalid } from './reveal-invalid';

const EMPTY_PROJECT: MakeProjectRequest = { name: '', display_name: '', description: '', public: false };

@Component({
  selector: 'app-create-project-dialog',
  standalone: true,
  imports: [FormsModule, ButtonComponent, DialogComponent, FormFieldComponent, InputDirective, SelectComponent, SubmitDirective, NameFieldsComponent, OwnerTeamSelectComponent],
  changeDetection: ChangeDetectionStrategy.Eager,
  template: `
    <gr-dialog #projectDialog header="Create Project" [(visible)]="visible" width="500px" [busy]="creating()" [error]="error()" [confirmDiscard]="formChanges.changed()">
      <app-name-fields
        #nameFields
        noun="project"
        [names]="names"
        [submitAttempted]="submitAttempted()"
        [(displayName)]="project.display_name"
        [(name)]="project.name"
        (submitted)="create()"
        (nameFieldsChanged)="error.set(null)"
      />
      <gr-form-field label="Description" for="project-description">
        <textarea grInput id="project-description" [(ngModel)]="project.description" placeholder="Describe your project..." rows="3"></textarea>
      </gr-form-field>
      <gr-form-field label="Visibility" for="project-visibility" hint="Public: Everyone can view its builds, tasks and logs, even visitors who aren't signed in.">
        <gr-select
          inputId="project-visibility"
          [options]="visibilityOptions"
          optionLabel="label"
          optionValue="value"
          [(ngModel)]="project.public"
        />
      </gr-form-field>
      @if (visible()) {
        <app-owner-team-select memberPlace="project" [preselectedOwnerTeam]="preselectedOwnerTeam()" [(selectedOwnerTeam)]="project.owner_team" />
      }
      <div grDialogFooter>
        <button grButton label="Cancel" severity="secondary" (click)="projectDialog.close()" [disabled]="creating()"></button>
        <button grButton label="Create" (grSubmit)="create()" [disabled]="names.blocksSubmit() || creating()" [loading]="creating()"></button>
      </div>
    </gr-dialog>
  `,
})
export class CreateProjectDialogComponent {
  private projects = inject(ProjectsService);
  private notify = inject(NotifyService);

  visible = model(false);
  preselectedOwnerTeam = input<TeamNameAndDisplayName | null>(null);
  created = output<string>();

  protected readonly visibilityOptions = [
    { label: 'Private', value: false },
    { label: 'Public', value: true },
  ];
  protected names = nameAvailability((name) => this.projects.checkProjectNameAvailable(name));
  protected project: MakeProjectRequest = { ...EMPTY_PROJECT };
  protected creating = signal(false);
  protected error = signal<string | null>(null);
  protected submitAttempted = signal(false);
  private nameFields = viewChild('nameFields', { read: ElementRef<HTMLElement> });
  private ownerTeamSelect = viewChild(OwnerTeamSelectComponent);
  private revealFirstInvalid = injectRevealFirstInvalid();
  protected formChanges = dialogFormChangesSinceOpen(() => this.project);

  constructor() {
    effect(() => {
      if (!this.visible()) return;
      untracked(() => {
        this.project = { ...EMPTY_PROJECT };
        this.names.reset();
        this.submitAttempted.set(false);
        this.error.set(null);
        this.formChanges.mark();
      });
    });
  }

  protected create(): void {
    if (this.creating() || this.names.blocksSubmit()) return;
    this.submitAttempted.set(true);
    if (!this.project.name || !this.project.display_name) {
      this.revealFirstInvalid(() => this.nameFields()?.nativeElement);
      return;
    }
    this.creating.set(true);
    this.error.set(null);
    this.projects.createProject({ ...this.project, owner_team: this.ownerTeamSelect()?.ownerTeamNameToSend() ?? null }).subscribe({
      next: () => {
        this.creating.set(false);
        this.visible.set(false);
        this.notify.success('Project created');
        this.created.emit(this.project.name);
      },
      error: (error: unknown) => {
        this.error.set(apiErrorMessage(error, 'Failed to create project.'));
        this.creating.set(false);
      },
    });
  }
}
