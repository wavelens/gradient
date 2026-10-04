/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, input, output, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { RouterModule } from '@angular/router';
import { Observable } from 'rxjs';
import { TeamsService } from '@core/services/teams.service';
import { TeamGrant } from '@core/models';
import {
  AutoCompleteComponent,
  BadgeComponent,
  ButtonComponent,
  DialogComponent,
  EmptyStateComponent,
  FormFieldComponent,
  MessageBannerComponent,
  RowComponent,
  RowListComponent,
  SelectComponent,
  SettingsSectionComponent,
} from '@gradient/ui/ui';

export type GrantedPart = 'users' | 'workers';

interface GrantForm {
  team: string;
  role: string;
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
  grants = input<GrantedPart>('users');
  roles = input<string[]>([]);
  canEdit = input(false);
  changed = output<void>();

  part = computed<GrantedPart>(() => (this.kind() === 'cache' ? 'users' : this.grants()));
  roleOptions = computed(() => this.roles().map((role) => ({ label: role, value: role })));
  allGrants = signal<TeamGrant[]>([]);
  shownGrants = computed(() => this.allGrants().filter((grant) => grant[this.part()]));
  suggestions = signal<string[]>([]);
  error = signal<string | null>(null);
  busy = signal<string | null>(null);
  showGrant = signal(false);
  form: GrantForm = { team: '', role: '' };

  ngOnInit(): void {
    this.load();
  }

  load(): void {
    const grants = this.kind() === 'project' ? this.teams.projectGrants(this.name()) : this.teams.cacheGrants(this.name());
    grants.subscribe({
      next: (items) => this.allGrants.set(items),
      error: () => this.allGrants.set([]),
    });
  }

  search(event: { query: string }): void {
    this.teams.list().subscribe({
      next: (mine) => this.suggestions.set(mine.map((t) => t.name).filter((n) => n.includes(event.query))),
      error: () => this.suggestions.set([]),
    });
  }

  openGrant(): void {
    this.form = { team: '', role: this.roles()[0] ?? '' };
    this.error.set(null);
    this.showGrant.set(true);
  }

  grant(): void {
    if (!this.form.team) return;
    const request =
      this.kind() === 'project'
        ? this.grantProject(this.form)
        : this.teams.grantCache(this.name(), this.form.team, this.form.role);
    this.run('grant', request, () => this.showGrant.set(false));
  }

  remove(grant: TeamGrant): void {
    const request = this.kind() === 'project' ? this.removeFromProject(grant) : this.teams.removeCacheGrant(this.name(), grant.team);
    this.run(grant.team, request);
  }

  private grantProject({ team, role }: GrantForm): Observable<string> {
    const existing = this.allGrants().some((grant) => grant.team === team && !grant.pending);
    const users = this.part() === 'users';
    if (existing) {
      return this.teams.updateProjectGrant(this.name(), team, users ? { users: true, role } : { workers: true });
    }
    return this.teams.grantProject(
      this.name(),
      users ? { team, role, users: true, workers: false } : { team, users: false, workers: true },
    );
  }

  private removeFromProject(grant: TeamGrant): Observable<string> {
    const keepsOtherPart = this.part() === 'users' ? grant.workers : grant.users;
    if (keepsOtherPart && !grant.pending) {
      return this.teams.updateProjectGrant(this.name(), grant.team, { [this.part()]: false });
    }
    return this.teams.removeProjectGrant(this.name(), grant.team);
  }

  private run(key: string, request: Observable<string>, done?: () => void): void {
    this.busy.set(key);
    this.error.set(null);
    request.subscribe({
      next: () => {
        this.busy.set(null);
        done?.();
        this.load();
        this.changed.emit();
      },
      error: (err: Error) => {
        this.busy.set(null);
        this.error.set(err.message || 'The team change failed.');
      },
    });
  }
}
