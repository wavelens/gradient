/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, effect, inject, input, model, output, signal, untracked } from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { FormsModule } from '@angular/forms';
import {
  AutoCompleteComponent,
  ButtonComponent,
  DialogComponent,
  FormFieldComponent,
  SelectComponent,
  SubmitDirective,
  type AutoCompleteSuggestion,
} from '@gradient/ui/ui';
import { AuthService } from '@core/services/auth.service';
import { UserService } from '@core/services/user.service';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import { formatMemberNameForAutocompleteSuggestion, type MemberPlace } from '@shared/access';
import { dialogFormChangesSinceOpen } from '@shared/forms/unsaved-changes';
import {
  type AddNowOrInviteChoice,
  type PeopleListRow,
  getInviteDialogPreviewSentence,
  usernameHasTeamGrantOrOwnerTeamAccessToProjectOrCache,
} from './access-reason-label';
import type { MembersPageRequests } from './members-page-requests';
import { Subject, catchError, debounceTime, map, of, switchMap } from 'rxjs';

const usernameSearchWaitAfterLastKeystrokeMilliseconds = 250;

@Component({
  selector: 'app-invite-person-dialog',
  standalone: true,
  imports: [FormsModule, AutoCompleteComponent, ButtonComponent, DialogComponent, FormFieldComponent, SelectComponent, SubmitDirective],
  changeDetection: ChangeDetectionStrategy.Eager,
  template: `
    <gr-dialog #inviteDialog header="Invite Member" [(visible)]="visible" width="460px" [error]="addError()" [busy]="busy()" [confirmDiscard]="formChanges.changed()">
      <gr-form-field label="Username" for="invite-person-user" [invalid]="!!userError()" [error]="userError() ?? ''">
        <gr-autocomplete
          inputId="invite-person-user"
          [ngModel]="user()"
          (ngModelChange)="user.set($event); addError.set(null)"
          [suggestions]="suggestions()"
          (completeMethod)="search($event)"
          [forceSelection]="false"
          (enter)="addMember()"
        />
      </gr-form-field>
      <gr-form-field label="Role" for="invite-person-role">
        <gr-select inputId="invite-person-role" [ngModel]="role()" (ngModelChange)="role.set($event)" [options]="roleOptions()" optionLabel="label" optionValue="value" />
      </gr-form-field>
      <fieldset class="gr-vertical-radio-button-fieldset">
        <legend>Add method</legend>
        <label>
          <input type="radio" name="invite-person-choice" value="add-now" [checked]="inviteOrAddNowChoice() === 'add-now'"
            [disabled]="!canOfferAddNowForTypedUsername()" (change)="selectedInviteOption.set('add-now')" />
          Add now
        </label>
        @if (addNowDisabledExplanation(); as explanation) {
          <p class="gr-form-aside">{{ explanation }}</p>
        }
        <label>
          <input type="radio" name="invite-person-choice" value="invite" [checked]="inviteOrAddNowChoice() === 'invite'"
            (change)="selectedInviteOption.set('invite')" />
          Send invitation
        </label>
      </fieldset>
      @if (inviteOutcomeSentence(); as sentence) {
        <p>{{ sentence }}</p>
      }
      <div grDialogFooter>
        <button grButton label="Cancel" severity="secondary" [disabled]="busy()" (click)="inviteDialog.close()"></button>
        <button grButton [label]="inviteOrAddNowChoice() === 'add-now' ? 'Add' : 'Invite'" [loading]="busy()" (grSubmit)="addMember()"></button>
      </div>
    </gr-dialog>
  `,
})
export class InvitePersonDialogComponent {
  private auth = inject(AuthService);
  private users = inject(UserService);
  private notify = inject(NotifyService);

  visible = model(false);
  kind = input.required<MemberPlace>();
  requests = input.required<MembersPageRequests>();
  roleOptions = input.required<readonly { label: string; value: string }[]>();
  rows = input<readonly PeopleListRow[]>([]);
  personAddedOrInvited = output<void>();

  protected user = signal('');
  protected role = signal('');
  protected selectedInviteOption = signal<AddNowOrInviteChoice>('invite');
  protected suggestions = signal<AutoCompleteSuggestion[]>([]);
  protected addError = signal<string | null>(null);
  protected busy = signal(false);
  protected addAttempted = signal(false);
  protected formChanges = dialogFormChangesSinceOpen(() => [this.user(), this.role(), this.selectedInviteOption()]);

  protected canOfferAddNowForTypedUsername = computed(
    () =>
      this.auth.user()?.superuser === true ||
      (this.kind() !== 'team' && usernameHasTeamGrantOrOwnerTeamAccessToProjectOrCache(this.rows(), this.user().trim())),
  );
  protected inviteOrAddNowChoice = computed<AddNowOrInviteChoice>(() =>
    this.canOfferAddNowForTypedUsername() ? this.selectedInviteOption() : 'invite',
  );
  protected addNowDisabledExplanation = computed(() => {
    if (this.canOfferAddNowForTypedUsername()) return '';
    return this.kind() === 'team'
      ? 'Only instance administrators can add people to a team right away.'
      : `Only instance administrators can add people immediately. Those people don't have access to this ${this.kind()} through a team yet.`;
  });
  protected inviteOutcomeSentence = computed(() => {
    const user = this.user().trim();
    if (!user || !this.role()) return '';
    const label = this.roleOptions().find((option) => option.value === this.role())?.label ?? this.role();
    return getInviteDialogPreviewSentence(user, label, this.inviteOrAddNowChoice());
  });
  protected userError = computed(() => (this.addAttempted() && !this.user().trim() ? 'Enter a username.' : null));

  private usernameAutocompleteText$ = new Subject<string>();

  constructor() {
    this.usernameAutocompleteText$
      .pipe(
        debounceTime(usernameSearchWaitAfterLastKeystrokeMilliseconds),
        switchMap((query) =>
          query.trim()
            ? this.users.searchUsers(query).pipe(
                map((found) => {
                  const directMembers = new Set(this.rows().filter((row) => row.directRole).map((row) => row.user));
                  return found.filter((user) => !directMembers.has(user.username)).map(formatMemberNameForAutocompleteSuggestion);
                }),
                catchError(() => of([])),
              )
            : of([]),
        ),
        takeUntilDestroyed(),
      )
      .subscribe((suggestions) => this.suggestions.set(suggestions));
    effect(() => {
      if (!this.visible()) return;
      untracked(() => {
        this.user.set('');
        this.role.set(this.roleOptions()[0]?.value ?? '');
        this.selectedInviteOption.set('invite');
        this.addError.set(null);
        this.addAttempted.set(false);
        this.formChanges.mark();
      });
    });
  }

  protected search(event: { query: string }): void {
    this.usernameAutocompleteText$.next(event.query);
  }

  protected addMember(): void {
    if (this.busy()) return;
    this.addAttempted.set(true);
    if (this.userError() || !this.role()) return;
    const addNow = this.inviteOrAddNowChoice() === 'add-now';
    this.busy.set(true);
    this.addError.set(null);
    this.requests().addMember(this.user().trim(), this.role(), addNow).subscribe({
      next: () => {
        this.busy.set(false);
        this.visible.set(false);
        this.notify.success(addNow ? 'Member added' : 'Invitation sent');
        this.personAddedOrInvited.emit();
      },
      error: (error: unknown) => {
        this.busy.set(false);
        this.addError.set(apiErrorMessage(error, "Couldn't add member"));
      },
    });
  }
}
