/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, inject, input, output, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Observable, finalize } from 'rxjs';
import { ButtonComponent, DialogComponent, MessageBannerComponent, NameFieldComponent, RowComponent, SubmitDirective } from '@gradient/ui/ui';
import { AccessState } from '@core/models/access.model';
import { ApiError, apiErrorMessage } from '@core/services/api.service';
import { NotifyService } from '@core/services/notify.service';
import { ManagedDisableDirective } from '@shared/access';
import { NameAvailability, maxSlugNameCharacterCount } from './name-availability';

@Component({
  selector: 'app-rename-url-name',
  standalone: true,
  imports: [FormsModule, ButtonComponent, DialogComponent, MessageBannerComponent, NameFieldComponent, RowComponent, SubmitDirective, ManagedDisableDirective],
  changeDetection: ChangeDetectionStrategy.Eager,
  styles: `
    .managed-note {
      display: block;
      margin-top: 0.25rem;
    }
  `,
  template: `
    <gr-row>
      Rename {{ title() }}
      <span slot="meta">
        Rename the URL slug. Old links stop working.
        @if (access().managed) {
          <span class="managed-note">
            Entries in the declarative state file define this {{ noun() }}. Change the name in the state file and keep the id
            <code>{{ entityUuidToKeepOnRename() }}</code>. Gradient can then rename the {{ noun() }} instead of replacing it.
          </span>
        } @else if (settingsFormHasUnsavedChanges()) {
          <span class="managed-note">Save or discard the unsaved changes above before renaming.</span>
        }
      </span>
      <button
        slot="actions"
        grButton
        label="Rename"
        icon="edit"
        severity="danger"
        [appManagedDisable]="access()"
        [disabled]="settingsFormHasUnsavedChanges()"
        (click)="openRenameDialogWithCurrentUrlName()"
      ></button>
    </gr-row>

    <gr-dialog
      #renameDialog
      [header]="'Rename ' + title()"
      [(visible)]="isRenameDialogOpen"
      [busy]="isRenameRequestInFlight()"
      [error]="error()"
      width="480px"
    >
      <gr-message-banner type="warning">
        Links, bookmarks, API clients and Nix configurations that use the old name <code>{{ currentUrlName() }}</code> in a URL stop working.
        @if (noun() === 'cache') {
          Signing key names also change. Replace the old entry in <code>trusted-public-keys</code> with the new public key from the cache settings.
        }
      </gr-message-banner>
      <gr-name-field
        label="New name"
        [inputId]="noun() + '-new-name'"
        [maxLength]="maxSlugNameCharacterCount"
        [state]="names().state()"
        [ngModel]="newUrlName()"
        (ngModelChange)="updateNameClearErrorAndCheckAvailability($event)"
        (keyup.enter)="sendRenameRequestHandleConflictCloseDialogAndEmitNewName()"
      />
      <div grDialogFooter>
        <button grButton label="Cancel" severity="secondary" [disabled]="isRenameRequestInFlight()" (click)="renameDialog.close()"></button>
        <button
          grButton
          label="Rename"
          severity="danger"
          [disabled]="!canSubmit()"
          [loading]="isRenameRequestInFlight()"
          (grSubmit)="sendRenameRequestHandleConflictCloseDialogAndEmitNewName()"
        ></button>
      </div>
    </gr-dialog>
  `,
})
export class RenameUrlNameComponent {
  private notify = inject(NotifyService);

  noun = input.required<'project' | 'cache' | 'task'>();
  currentUrlName = input.required<string>();
  entityUuidToKeepOnRename = input.required<string>();
  access = input.required<AccessState>();
  names = input.required<NameAvailability>();
  sendRenameRequestToServer = input.required<(name: string) => Observable<unknown>>();
  settingsFormHasUnsavedChanges = input(false);
  urlNameRenameAccepted = output<string>();

  protected readonly maxSlugNameCharacterCount = maxSlugNameCharacterCount;
  protected title = computed(() => this.noun().charAt(0).toUpperCase() + this.noun().slice(1));
  protected isRenameDialogOpen = signal(false);
  protected newUrlName = signal('');
  protected isRenameRequestInFlight = signal(false);
  protected error = signal<string | null>(null);
  protected canSubmit = computed(
    () =>
      !this.isRenameRequestInFlight() &&
      !!this.newUrlName() &&
      this.newUrlName() !== this.currentUrlName() &&
      !this.names().blocksSubmit(),
  );

  openRenameDialogWithCurrentUrlName(): void {
    this.newUrlName.set(this.currentUrlName());
    this.names().reset();
    this.error.set(null);
    this.isRenameDialogOpen.set(true);
  }

  updateNameClearErrorAndCheckAvailability(name: string): void {
    this.newUrlName.set(name);
    this.error.set(null);
    this.names().check(name === this.currentUrlName() ? '' : name);
  }

  sendRenameRequestHandleConflictCloseDialogAndEmitNewName(): void {
    if (!this.canSubmit()) return;
    const name = this.newUrlName();
    this.isRenameRequestInFlight.set(true);
    this.error.set(null);
    this.sendRenameRequestToServer()(name)
      .pipe(finalize(() => this.isRenameRequestInFlight.set(false)))
      .subscribe({
        next: () => {
          this.isRenameDialogOpen.set(false);
          this.notify.success(`${this.title()} renamed`);
          this.urlNameRenameAccepted.emit(name);
        },
        error: (error: unknown) => {
          if (error instanceof ApiError && error.status === 409) this.names().markTaken();
          else this.error.set(apiErrorMessage(error, 'Rename failed.'));
        },
      });
  }
}
