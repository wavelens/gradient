/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, inject, input, signal } from '@angular/core';
import { tap } from 'rxjs';
import { ButtonComponent, LoadStateComponent, RowComponent, RowListComponent, SettingsSectionComponent } from '@gradient/ui/ui';
import type { AccessState, CacheResponse, PendingInvitationItem } from '@core/models';
import { NotifyService } from '@core/services/notify.service';
import { ConfirmService } from '@shared/confirm';
import { WritableDirective, formatMemberNameForConfirmDialog, teamRoleValueToDisplayLabel, type MemberPlace } from '@shared/access';
import { PENDING_INVITATION_REVOKE_FAILURE, getPendingInvitationRevocationConsequenceSentence } from '@shared/access/pending-invitation-revoke-dialog-texts';
import { ServerTimestampToLocalDateTimePipe } from '@shared/text';
import { EntityNameComponent } from '@shared/ui';
import { runReloadIntervalWhileTabVisible } from '@shared/paging';
import { TeamGrantsComponent } from '@features/teams/team-grants/team-grants.component';
import type { MembersPageRequests } from './members-page-requests';

@Component({
  selector: 'app-pending-invitations-and-team-grant-requests',
  standalone: true,
  imports: [
    ButtonComponent,
    EntityNameComponent,
    LoadStateComponent,
    RowComponent,
    RowListComponent,
    SettingsSectionComponent,
    WritableDirective,
    ServerTimestampToLocalDateTimePipe,
    TeamGrantsComponent,
  ],
  changeDetection: ChangeDetectionStrategy.Eager,
  template: `
    <gr-settings-section title="Pending Invitations" [card]="false">
      <gr-load-state
        [loading]="loading()"
        [error]="loadError()"
        [empty]="invitations().length === 0"
        loadingMessage="Loading invitations..."
        errorTitle="Couldn't load invitations"
        emptyIcon="mail"
        emptyTitle="No Pending Invitations"
        (retry)="load()"
      >
        <gr-row-list>
          @for (invitation of invitations(); track invitation.user) {
            <gr-row icon="mail">
              <gr-entity-name [displayName]="invitation.name" [name]="invitation.user" />
              <span slot="meta">Role {{ teamRoleValueToDisplayLabel(invitation.role) }} · Expires {{ invitation.expires_at | serverDate }}</span>
              <button
                slot="actions"
                *appWritable="access()"
                grButton
                label="Revoke"
                icon="close"
                severity="danger"
                [attr.aria-label]="'Revoke invitation for ' + invitation.user"
                (click)="revokeInvitation(invitation)"
              ></button>
            </gr-row>
          }
        </gr-row-list>
      </gr-load-state>
    </gr-settings-section>
    @let grantKind = kind();
    @if (grantKind !== 'team') {
      <app-team-grants
        [kind]="grantKind"
        [name]="name()"
        [roles]="roleNames()"
        [cacheWithViewerPermissions]="cache()"
        [canEdit]="access().canEdit && !access().managed"
        whichGrantsToShow="pending"
      />
    }
  `,
})
export class PendingInvitationsAndTeamGrantRequestsComponent implements OnInit {
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);
  protected readonly teamRoleValueToDisplayLabel = teamRoleValueToDisplayLabel;

  kind = input.required<MemberPlace>();
  name = input.required<string>();
  requests = input.required<MembersPageRequests>();
  access = input.required<AccessState>();
  roleNames = input<string[]>([]);
  cache = input<CacheResponse | null>(null);

  invitations = signal<PendingInvitationItem[]>([]);
  loading = signal(true);
  loadError = signal<unknown>(null);

  constructor() {
    runReloadIntervalWhileTabVisible(() =>
      this.requests().invitations().pipe(tap((invitations) => this.invitations.set(invitations))),
    );
  }

  ngOnInit(): void {
    this.load();
  }

  load(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.requests().invitations().subscribe({
      next: (invitations) => {
        this.invitations.set(invitations);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  revokeInvitation(invitation: PendingInvitationItem): void {
    this.confirm
      .confirm({
        title: 'Revoke invitation',
        target: formatMemberNameForConfirmDialog(invitation.name, invitation.user),
        consequence: getPendingInvitationRevocationConsequenceSentence(invitation.user),
        confirmLabel: 'Revoke',
        failure: PENDING_INVITATION_REVOKE_FAILURE,
        action: () => this.requests().revokeInvitation(invitation.user),
      })
      .subscribe(() => {
        this.invitations.update((invitations) => invitations.filter((entry) => entry.user !== invitation.user));
        this.notify.success('Invitation revoked');
      });
  }
}
