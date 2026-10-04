/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, input, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { RouterModule } from '@angular/router';
import { Observable } from 'rxjs';
import { TeamsService } from '@core/services/teams.service';
import { TeamGrant } from '@core/models';
import {
  AutoCompleteComponent,
  BadgeComponent,
  ButtonComponent,
  CheckboxComponent,
  DialogComponent,
  EmptyStateComponent,
  FormFieldComponent,
  MessageBannerComponent,
  RowComponent,
  RowListComponent,
  SelectComponent,
  SettingsSectionComponent,
} from '@gradient/ui/ui';

interface GrantForm {
  team: string;
  role: string;
  users: boolean;
  workers: boolean;
}

@Component({
  selector: 'app-team-grants',
  standalone: true,
  imports: [
    FormsModule,
    RouterModule,
    AutoCompleteComponent,
    BadgeComponent,
    ButtonComponent,
    CheckboxComponent,
    DialogComponent,
    EmptyStateComponent,
    FormFieldComponent,
    MessageBannerComponent,
    RowComponent,
    RowListComponent,
    SelectComponent,
    SettingsSectionComponent,
  ],
  templateUrl: './team-grants.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class TeamGrantsComponent implements OnInit {
  private teams = inject(TeamsService);

  kind = input.required<'project' | 'cache'>();
  name = input.required<string>();
  roles = input<string[]>([]);
  canEdit = input(false);

  roleOptions = computed(() => this.roles().map((role) => ({ label: role, value: role })));
  grants = signal<TeamGrant[]>([]);
  suggestions = signal<string[]>([]);
  error = signal<string | null>(null);
  busy = signal<string | null>(null);
  showGrant = signal(false);
  form: GrantForm = { team: '', role: '', users: true, workers: false };

  ngOnInit(): void {
    this.load();
  }

  load(): void {
    const grants = this.kind() === 'project' ? this.teams.projectGrants(this.name()) : this.teams.cacheGrants(this.name());
    grants.subscribe({
      next: (items) => this.grants.set(items),
      error: () => this.grants.set([]),
    });
  }

  search(event: { query: string }): void {
    this.teams.list().subscribe({
      next: (mine) => this.suggestions.set(mine.map((t) => t.name).filter((n) => n.includes(event.query))),
      error: () => this.suggestions.set([]),
    });
  }

  openGrant(): void {
    this.form = { team: '', role: this.roles()[0] ?? '', users: true, workers: this.kind() === 'project' };
    this.error.set(null);
    this.showGrant.set(true);
  }

  grant(): void {
    if (!this.form.team) return;
    const request: Observable<string> =
      this.kind() === 'project'
        ? this.teams.grantProject(this.name(), {
            team: this.form.team,
            role: this.form.users ? this.form.role : undefined,
            users: this.form.users,
            workers: this.form.workers,
          })
        : this.teams.grantCache(this.name(), this.form.team, this.form.role);
    this.run('grant', request, () => this.showGrant.set(false));
  }

  toggleWorkers(grant: TeamGrant): void {
    this.run(grant.team, this.teams.updateProjectGrant(this.name(), grant.team, { workers: !grant.workers }));
  }

  remove(grant: TeamGrant): void {
    const request =
      this.kind() === 'project'
        ? this.teams.removeProjectGrant(this.name(), grant.team)
        : this.teams.removeCacheGrant(this.name(), grant.team);
    this.run(grant.team, request);
  }

  private run(key: string, request: Observable<string>, done?: () => void): void {
    this.busy.set(key);
    this.error.set(null);
    request.subscribe({
      next: () => {
        this.busy.set(null);
        done?.();
        this.load();
      },
      error: (err: Error) => {
        this.busy.set(null);
        this.error.set(err.message || 'The team change failed.');
      },
    });
  }
}
