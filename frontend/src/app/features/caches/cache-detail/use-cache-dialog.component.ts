/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, effect, inject, input, model, signal, untracked } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Router, RouterLink } from '@angular/router';
import { ButtonComponent, DialogComponent, FormFieldComponent, LoadStateComponent, SelectComponent } from '@gradient/ui/ui';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { ProjectsService } from '@core/services/projects.service';
import type { ProjectSummary } from '@core/models';
import { links } from '@shared/routing';

@Component({
  selector: 'app-use-cache-dialog',
  standalone: true,
  imports: [FormsModule, RouterLink, ButtonComponent, DialogComponent, FormFieldComponent, LoadStateComponent, SelectComponent],
  changeDetection: ChangeDetectionStrategy.Eager,
  template: `
    <gr-dialog header="Use with project" [(visible)]="visible" width="460px">
      <p>Select a project to open its cache subscription with this cache selected.</p>
      <gr-load-state [loading]="loading()" [error]="error()" [empty]="projects().length === 0" flat
                     errorTitle="Failed to load projects" (retry)="load()"
                     emptyIcon="folder" emptyTitle="No projects yet.">
        @if (canCreateProject()) {
          <a slot="empty-action" grButton label="Create project" icon="add" [routerLink]="links.projects()"></a>
        }
        <gr-form-field label="Project" for="use-cache-project">
          <gr-select inputId="use-cache-project" [options]="projects()" optionLabel="display_name" optionValue="name"
                     [(ngModel)]="project" />
        </gr-form-field>
      </gr-load-state>
      <div grDialogFooter>
        <button grButton label="Cancel" severity="secondary" (click)="visible.set(false)"></button>
        <button grButton label="Continue" icon="arrow_forward" [disabled]="!project" (click)="continue()"></button>
      </div>
    </gr-dialog>
  `,
})
export class UseCacheDialogComponent {
  protected readonly links = links;
  private projectsService = inject(ProjectsService);
  private router = inject(Router);
  private config = inject(ConfigService);
  private auth = inject(AuthService);

  visible = model(false);
  cache = input.required<string>();

  protected loading = signal(false);
  protected error = signal<unknown>(null);
  protected projects = signal<ProjectSummary[]>([]);
  protected project = '';
  protected canCreateProject = computed(() => this.config.canCreate(this.config.createProject, this.auth.user()?.superuser === true));

  constructor() {
    effect(() => {
      if (this.visible()) untracked(() => this.load());
    });
  }

  load(): void {
    this.loading.set(true);
    this.error.set(null);
    this.projectsService.getProjects(1, 100).subscribe({
      next: (page) => {
        const writable = page.items.filter((p) => !!p.role && p.role !== 'View' && !p.managed);
        this.projects.set(writable);
        this.project = writable.length === 1 ? writable[0].name : '';
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.error.set(error);
        this.loading.set(false);
      },
    });
  }

  protected continue(): void {
    if (!this.project) return;
    this.visible.set(false);
    this.router.navigateByUrl(links.subscribeToCache(this.project, this.cache()));
  }
}
