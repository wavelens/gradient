/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, effect, inject, model, output, signal, untracked } from '@angular/core';
import { ButtonComponent, DialogComponent } from '@gradient/ui/ui';
import { TeamsService } from '@core/services/teams.service';
import { apiErrorMessage } from '@core/services/api.service';
import { nameAvailability } from './name-availability';
import { NameFieldsComponent } from './name-fields.component';

@Component({
  selector: 'app-create-team-dialog',
  standalone: true,
  imports: [ButtonComponent, DialogComponent, NameFieldsComponent],
  changeDetection: ChangeDetectionStrategy.Eager,
  template: `
    <gr-dialog header="New Team" [(visible)]="visible" width="420px" [error]="error()" [busy]="creating()">
      <app-name-fields
        noun="team"
        [names]="names"
        [(displayName)]="form.displayName"
        [(name)]="form.name"
        (submitted)="create()"
        (nameFieldsChanged)="error.set(null)"
      />
      <div grDialogFooter>
        <button grButton label="Cancel" severity="secondary" [disabled]="creating()" (click)="visible.set(false)"></button>
        <button grButton label="Create" [loading]="creating()" [disabled]="!canSubmit()" (click)="create()"></button>
      </div>
    </gr-dialog>
  `,
})
export class CreateTeamDialogComponent {
  private teams = inject(TeamsService);

  visible = model(false);
  created = output<string>();

  protected names = nameAvailability((name) => this.teams.checkTeamNameAvailable(name));
  protected form = { name: '', displayName: '' };
  protected creating = signal(false);
  protected error = signal<string | null>(null);

  constructor() {
    effect(() => {
      if (!this.visible()) return;
      untracked(() => {
        this.form = { name: '', displayName: '' };
        this.names.reset();
        this.error.set(null);
      });
    });
  }

  protected canSubmit(): boolean {
    return !!this.form.displayName && !!this.form.name && !this.names.blocksSubmit() && !this.creating();
  }

  protected create(): void {
    if (!this.canSubmit()) return;
    this.creating.set(true);
    this.error.set(null);
    this.teams.create(this.form.name, this.form.displayName).subscribe({
      next: () => {
        this.creating.set(false);
        this.visible.set(false);
        this.created.emit(this.form.name);
      },
      error: (err: unknown) => {
        this.error.set(apiErrorMessage(err, 'Failed to create the team.'));
        this.creating.set(false);
      },
    });
  }
}
