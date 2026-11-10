/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, input, output } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { BadgeComponent, ButtonComponent, RowComponent, RowListComponent, SelectComponent, TooltipDirective } from '@gradient/ui/ui';
import type { AccessState } from '@core/models';
import { ManagedDisableDirective, teamRoleValueToDisplayLabel } from '@shared/access';
import type { PeopleListRow } from './access-reason-label';

@Component({
  selector: 'app-project-members-access-list',
  standalone: true,
  imports: [FormsModule, BadgeComponent, ButtonComponent, RowComponent, RowListComponent, SelectComponent, TooltipDirective, ManagedDisableDirective],
  changeDetection: ChangeDetectionStrategy.Eager,
  template: `
    <gr-row-list>
      @let pending = pendingMemberRoleSelection();
      @for (row of rows(); track row.user) {
        @let soleAdminReason = memberOnlyAdminDisabledReason()(row.user);
        <gr-row icon="person">
          <span class="mono">{{ row.user }}</span>
          <span slot="meta">@if (row.displayName && row.displayName !== row.user) { {{ row.displayName }} · }@for (source of row.sources; track $index) {@if (!$first) { · }{{ source.label }} }</span>
          <div slot="actions">
            @if (access().canEdit && !row.locked) {
              <gr-select
                [ngModel]="pending?.user === row.user ? pending.role : (row.directRole ?? row.role)"
                [options]="roleOptions()"
                optionLabel="label"
                optionValue="value"
                placeholder="No role"
                [disabled]="pending !== null || !!soleAdminReason"
                [appManagedDisable]="access()"
                [grTooltip]="soleAdminReason"
                [ariaLabel]="'Role: ' + row.user"
                (ngModelChange)="personRoleChange.emit({ user: row.user, role: $event })"
              />
              @if (row.directRole) {
                <button
                  grButton
                  label="Remove"
                  icon="close"
                  severity="danger"
                  [attr.aria-label]="'Remove ' + row.user"
                  [disabled]="pending !== null"
                  [disabledReason]="soleAdminReason"
                  [appManagedDisable]="access()"
                  (click)="removeMember.emit(row.user)"
                ></button>
              }
            } @else if (row.role) {
              <gr-badge severity="neutral">{{ teamRoleValueToDisplayLabel(row.role) }}</gr-badge>
            }
          </div>
        </gr-row>
      }
    </gr-row-list>
  `,
})
export class ProjectMembersAccessListComponent {
  protected readonly teamRoleValueToDisplayLabel = teamRoleValueToDisplayLabel;

  rows = input.required<readonly PeopleListRow[]>();
  roleOptions = input.required<readonly { label: string; value: string }[]>();
  access = input.required<AccessState>();
  pendingMemberRoleSelection = input<{ user: string; role: string } | null>(null);
  memberOnlyAdminDisabledReason = input<(user: string) => string>(() => '');
  personRoleChange = output<{ user: string; role: string }>();
  removeMember = output<string>();
}
