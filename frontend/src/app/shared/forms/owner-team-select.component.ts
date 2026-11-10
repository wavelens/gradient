/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, inject, input, model } from '@angular/core';
import { toSignal } from '@angular/core/rxjs-interop';
import { FormsModule } from '@angular/forms';
import { catchError, of } from 'rxjs';
import { FormFieldComponent, SelectComponent } from '@gradient/ui/ui';
import { TeamsService } from '@core/services/teams.service';
import type { TeamNameAndDisplayName, TeamSummary } from '@core/models';

@Component({
  selector: 'app-owner-team-select',
  standalone: true,
  imports: [FormsModule, FormFieldComponent, SelectComponent],
  changeDetection: ChangeDetectionStrategy.Eager,
  template: `
    @if (ownerTeamDropdownOptions().length > 1) {
      <gr-form-field label="Owner team" [for]="memberPlace() + '-owner-team'" [hint]="'Members of the owner team get the default role of the team on this ' + memberPlace() + '.'">
        <gr-select
          [inputId]="memberPlace() + '-owner-team'"
          [options]="ownerTeamDropdownOptions()"
          optionLabel="label"
          optionValue="value"
          [ngModel]="ownerTeamNameToSend()"
          (ngModelChange)="selectedOwnerTeam.set($event)"
        />
      </gr-form-field>
    }
  `,
})
export class OwnerTeamSelectComponent {
  memberPlace = input.required<'project' | 'cache'>();
  preselectedOwnerTeam = input<TeamNameAndDisplayName | null>(null);
  selectedOwnerTeam = model<string | null | undefined>(undefined);

  protected signedInUserTeams = toSignal(inject(TeamsService).list().pipe(catchError(() => of<TeamSummary[]>([]))), { initialValue: [] });

  protected ownerTeamDropdownOptions = computed(() => {
    const preselected = this.preselectedOwnerTeam();
    const teams: TeamNameAndDisplayName[] = this.signedInUserTeams();
    const choices = preselected && !teams.some((team) => team.name === preselected.name) ? [preselected, ...teams] : teams;
    return [{ label: 'No team', value: null as string | null }, ...choices.map((team) => ({ label: team.display_name || team.name, value: team.name }))];
  });

  readonly ownerTeamNameToSend = computed(() => {
    const selected = this.selectedOwnerTeam();
    if (selected !== undefined) return selected;
    const preselected = this.preselectedOwnerTeam();
    if (preselected) return preselected.name;
    const teams = this.signedInUserTeams();
    return teams.length === 1 ? teams[0].name : null;
  });
}
