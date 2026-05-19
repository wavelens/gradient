/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { forkJoin, of } from 'rxjs';
import { catchError } from 'rxjs/operators';
import { DialogModule } from 'primeng/dialog';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';
import { TextareaModule } from 'primeng/textarea';
import { ProjectsService } from '@core/services/projects.service';
import { OrganizationsService } from '@core/services/organizations.service';
import { IntegrationsService } from '@core/services/integrations.service';
import { AutoCompleteModule } from 'primeng/autocomplete';
import { SelectModule } from 'primeng/select';
import { CheckboxModule } from 'primeng/checkbox';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { WritableDirective, ManagedDisableDirective } from '@shared/access';
import { ConcurrencyPolicy, Integration, Project, ProjectIntegrationLink } from '@core/models';
import { injectProjectAccess } from '@core/resolvers/inject-access';

@Component({
  selector: 'app-project-settings',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    DialogModule,
    ButtonModule,
    InputTextModule,
    TextareaModule,
    AutoCompleteModule,
    SelectModule,
    CheckboxModule,
    LoadingSpinnerComponent,
    WritableDirective,
    ManagedDisableDirective,
  ],
  templateUrl: './project-settings.component.html',
  styleUrl: './project-settings.component.scss',
})
export class ProjectSettingsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private projectsService = inject(ProjectsService);
  private orgsService = inject(OrganizationsService);
  private integrationsService = inject(IntegrationsService);

  access = injectProjectAccess();

  loading = signal(true);
  saving = signal(false);
  deleting = signal(false);
  toggling = signal(false);
  transferring = signal(false);

  project = signal<Project | null>(null);
  showDeleteDialog = signal(false);
  showTransferDialog = signal(false);
  errorMessage = signal<string | null>(null);
  saveSuccess = signal(false);
  transferOrgName = '';
  transferError = signal<string | null>(null);
  transferSuccess = signal(false);
  transferOrgSuggestions = signal<string[]>([]);

  orgName = '';
  orgDisplayName = signal('');
  projectName = '';

  formData: {
    display_name: string;
    description: string;
    repository: string;
    wildcard: string;
    keep_evaluations: number;
    concurrency: ConcurrencyPolicy;
    sign_cache: boolean;
  } = {
    display_name: '',
    description: '',
    repository: '',
    wildcard: '',
    keep_evaluations: 30,
    concurrency: 'soft_abort',
    sign_cache: true,
  };

  concurrencyOptions: { label: string; value: ConcurrencyPolicy; disabled?: boolean }[] = [
    { label: 'Hard Abort: cancel running evaluation and its in-flight builds', value: 'hard_abort' },
    { label: 'Soft Abort: mark current evaluation aborted, let in-flight builds finish', value: 'soft_abort' },
    { label: 'Skip: keep the running evaluation, discard the new trigger event', value: 'skip' },
    { label: 'All: run a new evaluation alongside the in-flight one', value: 'all' },
  ];

  integrationsLoading = signal(true);
  savingIntegration = signal(false);
  integrationSaveSuccess = signal(false);
  integrationErrorMessage = signal<string | null>(null);
  availableIntegrations = signal<Integration[]>([]);
  projectIntegration = signal<ProjectIntegrationLink | null>(null);

  outboundSelection: string | null = null;

  outboundIntegrationOptions = signal<{ label: string; value: string | null }[]>([
    { label: 'None', value: null },
  ]);

  /// True when the project repository URL points at github.com but the org
  /// has no GitHub App outbound integration row — surfaces an install CTA.
  showGithubAppInstallHint = computed(() => {
    const repo = this.project()?.repository ?? '';
    if (!isGithubRepoUrl(repo)) return false;
    return !this.availableIntegrations().some(
      (i) => i.kind === 'outbound' && i.forge_type === 'github',
    );
  });

  ngOnInit(): void {
    this.orgName = this.route.snapshot.paramMap.get('org') || '';
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.orgsService.getOrganization(this.orgName).subscribe({
      next: (org) => this.orgDisplayName.set(org.display_name),
      error: () => {},
    });
    this.loadProject();
    this.loadIntegrations();
  }

  loadIntegrations(): void {
    this.integrationsLoading.set(true);
    forkJoin({
      list: this.integrationsService.listOrgIntegrations(this.orgName).pipe(
        catchError(() => of<Integration[]>([])),
      ),
      link: this.integrationsService.getProjectIntegration(this.orgName, this.projectName).pipe(
        catchError(() => of<ProjectIntegrationLink | null>(null)),
      ),
    }).subscribe(({ list, link }) => {
      this.availableIntegrations.set(list);
      const outbound: { label: string; value: string | null }[] = [{ label: 'None', value: null }];
      for (const i of list) {
        if (i.kind === 'outbound') outbound.push({ label: `${i.display_name} (${i.forge_type})`, value: i.id });
      }
      // The project may point at an integration the org no longer lists
      // (renamed, deleted, or never seeded). Surface it as a labelled option
      // so the dropdown shows something meaningful instead of a raw UUID,
      // and the user can switch to a known-good one without saving the
      // unresolved value back.
      const stored = link?.outbound_integration ?? null;
      if (stored && !outbound.some((o) => o.value === stored)) {
        outbound.push({
          label: `Unknown integration (${stored.slice(0, 8)}…) — please reselect`,
          value: stored,
        });
      }
      this.outboundIntegrationOptions.set(outbound);
      this.projectIntegration.set(link);
      this.outboundSelection = stored;
      this.integrationsLoading.set(false);
    });
  }

  saveIntegrationLink(): void {
    this.savingIntegration.set(true);
    this.integrationErrorMessage.set(null);
    this.integrationSaveSuccess.set(false);
    this.integrationsService
      .setProjectIntegration(this.orgName, this.projectName, {
        outbound_integration: this.outboundSelection,
      })
      .subscribe({
        next: (link) => {
          this.projectIntegration.set(link);
          this.savingIntegration.set(false);
          this.integrationSaveSuccess.set(true);
        },
        error: (error) => {
          this.integrationErrorMessage.set(error.message || 'Failed to save integration link.');
          this.savingIntegration.set(false);
        },
      });
  }

  loadProject(): void {
    this.loading.set(true);
    this.projectsService.getProjectInfo(this.orgName, this.projectName).subscribe({
      next: (project) => {
        if (project.name === 'build-request') {
          this.router.navigate(['/organization', this.orgName, 'project', project.name]);
          return;
        }
        this.project.set(project);
        this.formData = {
          display_name: project.display_name,
          description: project.description,
          repository: project.repository,
          wildcard: project.wildcard,
          keep_evaluations: project.keep_evaluations,
          concurrency: project.concurrency,
          sign_cache: project.sign_cache,
        };
        this.loading.set(false);
      },
      error: (error) => {
        console.error('Failed to load project:', error);
        this.loading.set(false);
      },
    });
  }

  saveSettings(): void {
    this.saving.set(true);
    this.errorMessage.set(null);
    this.saveSuccess.set(false);
    this.projectsService.updateProject(this.orgName, this.projectName, this.formData).subscribe({
      next: () => {
        this.saving.set(false);
        this.saveSuccess.set(true);
        this.loadProject();
      },
      error: (error) => {
        this.errorMessage.set(error.message || 'Failed to save settings.');
        this.saving.set(false);
      },
    });
  }

  toggleActive(): void {
    const proj = this.project();
    if (!proj) return;

    this.toggling.set(true);
    const action = proj.active
      ? this.projectsService.deactivateProject(this.orgName, this.projectName)
      : this.projectsService.activateProject(this.orgName, this.projectName);

    action.subscribe({
      next: () => {
        this.toggling.set(false);
        this.loadProject();
      },
      error: (error) => {
        console.error('Failed to toggle project status:', error);
        this.toggling.set(false);
      },
    });
  }

  onTransferOrgSearch(event: { query: string }): void {
    const q = event.query.trim().toLowerCase();
    this.orgsService.getOrganizations().subscribe({
      next: (res) => {
        const names = res.items
          .map((o) => o.name)
          .filter((n) => n !== this.orgName && (!q || n.toLowerCase().includes(q)));
        this.transferOrgSuggestions.set(names);
      },
      error: () => this.transferOrgSuggestions.set([]),
    });
  }

  onTransferDialogHide(): void {
    this.transferOrgName = '';
    this.transferError.set(null);
    this.transferSuccess.set(false);
  }

  transferOwnership(): void {
    if (!this.transferOrgName.trim()) return;
    this.transferring.set(true);
    this.transferError.set(null);
    this.transferSuccess.set(false);
    this.projectsService.transferOwnership(this.orgName, this.projectName, this.transferOrgName.trim()).subscribe({
      next: () => {
        this.transferring.set(false);
        this.transferSuccess.set(true);
        const targetOrg = this.transferOrgName.trim();
        this.transferOrgName = '';
        setTimeout(() => {
          this.showTransferDialog.set(false);
          this.router.navigate(['/organization', targetOrg]);
        }, 1500);
      },
      error: (error) => {
        this.transferError.set(error.message || 'Failed to transfer ownership.');
        this.transferring.set(false);
      },
    });
  }

  deleteProject(): void {
    this.deleting.set(true);
    this.projectsService.deleteProject(this.orgName, this.projectName).subscribe({
      next: () => {
        this.router.navigate(['/organization', this.orgName]);
      },
      error: (error) => {
        console.error('Failed to delete project:', error);
        this.deleting.set(false);
        this.showDeleteDialog.set(false);
      },
    });
  }
}

/// Mirrors the backend's host check (drop schemes/auth, then exact-match
/// `github.com` or `*.github.com`). Used only to decide whether to surface
/// the GitHub App install hint in the UI.
function isGithubRepoUrl(url: string): boolean {
  let rest = url.startsWith('git+') ? url.slice(4) : url;
  for (const scheme of ['https://', 'http://', 'git://', 'ssh://']) {
    if (rest.startsWith(scheme)) {
      rest = rest.slice(scheme.length);
      const host = rest.split(/[/:]/, 1)[0]?.toLowerCase() ?? '';
      return host === 'github.com' || host.endsWith('.github.com');
    }
  }
  // SCP-style: user@host:path
  const at = rest.indexOf('@');
  if (at >= 0 && rest.slice(at + 1).includes(':')) {
    const host = rest.slice(at + 1).split(/[/:]/, 1)[0]?.toLowerCase() ?? '';
    return host === 'github.com' || host.endsWith('.github.com');
  }
  return false;
}
