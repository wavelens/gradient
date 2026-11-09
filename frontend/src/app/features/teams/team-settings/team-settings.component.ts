/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, linkedSignal, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { TeamsService } from '@core/services/teams.service';
import { AuthService } from '@core/services/auth.service';
import { injectTeam } from '@core/resolvers/team-access.resolver';
import { NotifyService } from '@core/services/notify.service';
import { AccessState, TeamResponse, PatchTeamRequest } from '@core/models';
import { AccessService, ManagedDisableDirective, ServerConfigurationManagedBannerComponent, canManageTeam } from '@shared/access';
import { ConfirmService } from '@shared/confirm';
import { maxDisplayNameCharacterCount, unsavedChanges } from '@shared/forms';
import { CanComponentDeactivate } from '@core/guards/unsaved-changes.guard';
import {
  ButtonComponent,
  CheckboxComponent,
  FormFieldComponent,
  InputDirective,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
  SelectComponent,
  SettingsSectionComponent,
  SubmitDirective,
} from '@gradient/ui/ui';
import { links } from '@shared/routing';

interface SettingsForm {
  display_name: string;
  new_project_users: boolean;
  new_project_workers: boolean;
  new_project_role: string;
  oidc_group: string;
  scim_group: string;
}

@Component({
  selector: 'app-team-settings',
  standalone: true,
  imports: [
    FormsModule,
    RouterModule,
    ManagedDisableDirective, ServerConfigurationManagedBannerComponent,
    ButtonComponent,
    CheckboxComponent,
    FormFieldComponent,
    InputDirective,
    PageLayoutComponent,
    RowComponent,
    RowListComponent,
    SelectComponent,
    SettingsSectionComponent,
    SubmitDirective,
  ],
  templateUrl: './team-settings.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styles: `
    .save-hint {
      align-self: center;
    }
  `,
})
export class TeamSettingsComponent implements OnInit, CanComponentDeactivate {
  protected readonly links = links;
  protected readonly maxDisplayNameCharacterCount = maxDisplayNameCharacterCount;
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private teams = inject(TeamsService);
  private crumbs = inject(BreadcrumbsService);
  private authService = inject(AuthService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);
  protected accessService = inject(AccessService);

  readonly roleOptions = [
    { label: 'View', value: 'View' },
    { label: 'Write', value: 'Write' },
    { label: 'Admin', value: 'Admin' },
  ];

  private resolvedTeam = injectTeam();

  teamName = '';
  breadcrumb = computed(() => this.crumbs.team(this.teamName, { label: 'Settings' }));
  team = linkedSignal<TeamResponse | null>(() => this.resolvedTeam() ?? null);
  saving = signal(false);
  form: SettingsForm = this.formOf(null);
  readonly unsaved = unsavedChanges(() => this.form, 'Team settings');

  isSuperuser = computed(() => this.authService.user()?.superuser === true);
  access = computed(() => this.accessFor(canManageTeam(this.team(), this.authService.user())));

  ngOnInit(): void {
    this.teamName = this.route.snapshot.paramMap.get('team') || '';
    this.form = this.formOf(this.team());
    this.unsaved.mark();
  }

  canDeactivate() {
    return this.unsaved.canLeave();
  }

  get displayNameError(): string | null {
    return this.form.display_name.trim() ? null : 'Enter a display name.';
  }

  canSave(): boolean {
    return !this.saving() && this.accessService.isWritable(this.access()) && this.unsaved.changed() && !this.displayNameError;
  }

  setDefaultNewProjectRoleIfNoneChosen(grantUsers: boolean): void {
    if (grantUsers && !this.form.new_project_role) this.form.new_project_role = this.roleOptions[0].value;
  }

  save(): void {
    const team = this.team();
    if (!team || !this.canSave()) return;
    this.form.display_name = this.form.display_name.trim();
    const patch = this.changes(team);
    if (Object.keys(patch).length === 0) {
      this.unsaved.mark();
      return;
    }
    this.saving.set(true);
    this.teams.update(this.teamName, patch).subscribe({
      next: () => {
        this.team.set({ ...team, ...this.form });
        this.crumbs.rememberTeam(this.teamName, this.form.display_name);
        this.unsaved.mark();
        this.saving.set(false);
        this.notify.success('Team settings saved');
      },
      error: (err: unknown) => {
        this.saving.set(false);
        this.notify.failure('Failed to save team', err);
      },
    });
  }

  remove(): void {
    this.confirm
      .confirm({
        title: 'Delete team',
        target: this.team()?.display_name ?? this.teamName,
        consequence:
          'Delete the pending requests and invitations of the team, the access of the team to projects and caches, and the team workers. Connected team workers stop builds in those projects. Nothing can undo this action.',
        confirmLabel: 'Delete',
        failure: 'Failed to delete the team.',
        action: () => this.teams.remove(this.teamName),
      })
      .subscribe(() => {
        this.notify.success('Team deleted');
        this.router.navigateByUrl(links.teams());
      });
  }

  private accessFor(canEdit: boolean): AccessState {
    return { managed: this.team()?.managed ?? false, canEdit, canTrigger: canEdit };
  }

  private formOf(team: TeamResponse | null): SettingsForm {
    return {
      display_name: team?.display_name ?? '',
      new_project_users: team?.new_project_users ?? false,
      new_project_workers: team?.new_project_workers ?? false,
      new_project_role: team?.new_project_role ?? '',
      oidc_group: team?.oidc_group ?? '',
      scim_group: team?.scim_group ?? '',
    };
  }

  private changes(team: TeamResponse): PatchTeamRequest {
    const patch: PatchTeamRequest = {};
    if (this.form.display_name !== team.display_name) patch.display_name = this.form.display_name;
    if (this.isSuperuser()) {
      if (this.form.new_project_users !== team.new_project_users) patch.new_project_users = this.form.new_project_users;
      if (this.form.new_project_workers !== team.new_project_workers) {
        patch.new_project_workers = this.form.new_project_workers;
      }
      if (this.form.new_project_role !== (team.new_project_role ?? '')) {
        patch.new_project_role = this.form.new_project_role;
      }
      if (this.form.oidc_group !== (team.oidc_group ?? '')) patch.oidc_group = this.form.oidc_group;
      if (this.form.scim_group !== (team.scim_group ?? '')) patch.scim_group = this.form.scim_group;
    }
    return patch;
  }
}
