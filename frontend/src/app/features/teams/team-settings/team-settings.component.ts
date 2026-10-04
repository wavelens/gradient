/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { TeamsService } from '@core/services/teams.service';
import { AuthService } from '@core/services/auth.service';
import { PatchTeam, Team } from '@core/models';
import {
  ButtonComponent,
  CheckboxComponent,
  DialogComponent,
  FormFieldComponent,
  InputDirective,
  LoadingSpinnerComponent,
  MessageBannerComponent,
  PageLayoutComponent,
  RowComponent,
  SelectComponent,
  SettingsSectionComponent,
} from '@gradient/ui/ui';

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
    ButtonComponent,
    CheckboxComponent,
    DialogComponent,
    FormFieldComponent,
    InputDirective,
    LoadingSpinnerComponent,
    MessageBannerComponent,
    PageLayoutComponent,
    RowComponent,
    SelectComponent,
    SettingsSectionComponent,
  ],
  templateUrl: './team-settings.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class TeamSettingsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private teams = inject(TeamsService);
  private authService = inject(AuthService);

  readonly roleOptions = [
    { label: 'None', value: '' },
    { label: 'Admin', value: 'Admin' },
    { label: 'Write', value: 'Write' },
    { label: 'View', value: 'View' },
  ];

  teamName = '';
  team = signal<Team | null>(null);
  loading = signal(true);
  saving = signal(false);
  deleting = signal(false);
  saved = signal(false);
  error = signal<string | null>(null);
  showDelete = signal(false);
  form: SettingsForm = this.formOf(null);

  isSuperuser = computed(() => this.authService.user()?.superuser === true);

  ngOnInit(): void {
    this.teamName = this.route.snapshot.paramMap.get('team') || '';
    this.load();
  }

  load(): void {
    this.teams.get(this.teamName).subscribe({
      next: (team) => {
        this.team.set(team);
        this.form = this.formOf(team);
        this.loading.set(false);
      },
      error: (err: Error) => {
        this.error.set(err.message || 'Failed to load the team.');
        this.loading.set(false);
      },
    });
  }

  save(): void {
    const team = this.team();
    if (!team) return;
    const patch = this.changes(team);
    if (Object.keys(patch).length === 0) return;
    this.saving.set(true);
    this.saved.set(false);
    this.error.set(null);
    this.teams.update(this.teamName, patch).subscribe({
      next: () => {
        this.saving.set(false);
        this.saved.set(true);
        this.load();
      },
      error: (err: Error) => {
        this.saving.set(false);
        this.error.set(err.message || 'Failed to save the team.');
      },
    });
  }

  remove(): void {
    this.deleting.set(true);
    this.teams.remove(this.teamName).subscribe({
      next: () => this.router.navigate(['/teams']),
      error: (err: Error) => {
        this.deleting.set(false);
        this.showDelete.set(false);
        this.error.set(err.message || 'Failed to delete the team.');
      },
    });
  }

  private formOf(team: Team | null): SettingsForm {
    return {
      display_name: team?.display_name ?? '',
      new_project_users: team?.new_project_users ?? false,
      new_project_workers: team?.new_project_workers ?? false,
      new_project_role: team?.new_project_role ?? '',
      oidc_group: team?.oidc_group ?? '',
      scim_group: team?.scim_group ?? '',
    };
  }

  private changes(team: Team): PatchTeam {
    const patch: PatchTeam = {};
    if (this.form.display_name !== team.display_name) patch.display_name = this.form.display_name;
    if (this.form.new_project_users !== team.new_project_users) patch.new_project_users = this.form.new_project_users;
    if (this.form.new_project_workers !== team.new_project_workers) {
      patch.new_project_workers = this.form.new_project_workers;
    }
    if (this.form.new_project_role !== (team.new_project_role ?? '')) patch.new_project_role = this.form.new_project_role;
    if (this.isSuperuser()) {
      if (this.form.oidc_group !== (team.oidc_group ?? '')) patch.oidc_group = this.form.oidc_group;
      if (this.form.scim_group !== (team.scim_group ?? '')) patch.scim_group = this.form.scim_group;
    }
    return patch;
  }
}
