/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, input, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import {
  BadgeComponent,
  ButtonComponent,
  DialogComponent,
  EmptyStateComponent,
  FormFieldComponent,
  InputDirective,
  LoadStateComponent,
  MessageBannerComponent,
  SelectComponent,
  SelectOption,
  SubmitDirective,
  TabSwitchComponent,
} from '@gradient/ui/ui';
import { CreateRepositoryGrantRequest, GitHostConnectionOwner, RepositoryGrantResponse } from '@core/models';
import { GitHostConnectionsApi } from '@core/services/git-host-connections.service';
import { ProjectsService } from '@core/services/projects.service';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import { ConfirmService } from '@shared/confirm';

type RepositoryGrantCoverageChoice = 'one' | 'all';

interface AddRepositoryGrantForm {
  project: string | null;
  repository_owner: string;
  repositoryGrantRepositorySelection: RepositoryGrantCoverageChoice;
  repository_name: string;
}

const addGrantRepositoryOrAllRepositoriesTabSwitchOptions: readonly { label: string; value: RepositoryGrantCoverageChoice }[] = [
  { label: 'One repository', value: 'one' },
  { label: 'All repositories of the owner', value: 'all' },
];

const repositoryOwnerOrNameMaxLength = 255;
const plainPathSegmentPattern = /^[A-Za-z0-9._-]+$/;

function isPlainPathSegment(segment: string): boolean {
  return segment !== '.' && segment !== '..' && plainPathSegmentPattern.test(segment);
}

export function getGrantSourceLabel(source: string): string {
  switch (source) {
    case 'admin':
      return 'Added by admin';
    case 'upgrade':
      return 'Added by upgrade';
    case 'state_file':
      return 'Grant source: state file';
    default:
      return source;
  }
}

export function repositoryTextForGrant(grant: RepositoryGrantResponse): string {
  return grant.repository_name
    ? `${grant.repository_owner}/${grant.repository_name}`
    : 'All repositories of {owner}'.replace('{owner}', grant.repository_owner);
}

function emptyForm(): AddRepositoryGrantForm {
  return { project: null, repository_owner: '', repositoryGrantRepositorySelection: 'one', repository_name: '' };
}

@Component({
  selector: 'app-repository-grants',
  standalone: true,
  imports: [
    FormsModule,
    BadgeComponent,
    ButtonComponent,
    DialogComponent,
    EmptyStateComponent,
    FormFieldComponent,
    InputDirective,
    LoadStateComponent,
    MessageBannerComponent,
    SelectComponent,
    SubmitDirective,
    TabSwitchComponent,
  ],
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @if (canAddAndRemoveGrants()) {
      <div class="grants-head">
        <button grButton type="button" label="Add grant" icon="add" severity="secondary" size="small"
                (click)="resetGrantFormLoadProjectOptionsAndOpenAddGrantDialog()" data-testid="add-repository-grant"></button>
      </div>
    }
    <gr-load-state flat [loading]="loading()" [error]="loadError()" loadingMessage="Loading repository grants..."
                   errorTitle="Failed to load repository grants" (retry)="load()">
      @if (connectionRepositoryGrants().length === 0) {
        <gr-empty-state flat icon="lock" title="No repository grants"
                        message="No task can use this connection without a repository grant for the project." />
      } @else {
        <ul class="grants">
          @for (grant of connectionRepositoryGrants(); track grant.id) {
            <li data-testid="repository-grant">
              <span class="grant-project">{{ getGrantProjectLabel(grant) }}</span>
              <span class="mono grant-repository">{{ repositoryText(grant) }}</span>
              <gr-badge>{{ sourceLabel(grant.source) }}</gr-badge>
              @if (canAddAndRemoveGrants()) {
                <button grButton type="button" label="Delete" icon="delete" severity="danger" size="small"
                        [disabled]="!!grantRemoveDisabledReason(grant)" [attr.title]="grantRemoveDisabledReason(grant)"
                        (click)="removeGrantAfterConfirmation(grant)" data-testid="remove-repository-grant"></button>
              }
            </li>
          }
        </ul>
      }
    </gr-load-state>

    <gr-dialog header="Add grant" [visible]="addRepositoryGrantDialogOpen()"
               (visibleChange)="addRepositoryGrantDialogOpen.set($event)" width="520px">
      @if (addGrantFormApiErrorMessage(); as message) {
        <gr-message-banner type="error" data-testid="repository-grant-error">{{ message }}</gr-message-banner>
      }
      <gr-form-field label="Project" for="grant-project" required [invalid]="!!projectFieldValidationError()"
                     [error]="projectFieldValidationError() ?? ''"
                     [hint]="gitHostConnectionOwner().type === 'team' ? 'Only projects of this team show here.' : ''">
        <gr-select inputId="grant-project" [options]="projectOptionsForGrant()" optionLabel="label" optionValue="value"
                   placeholder="Select project" [ngModel]="form().project" (ngModelChange)="patchForm({ project: $event })" />
      </gr-form-field>
      <gr-form-field label="Repository owner" for="grant-repository-owner" required hint="User or organization on the Git host."
                     [invalid]="!!repositoryOwnerValidationError()" [error]="repositoryOwnerValidationError() ?? ''">
        <input grInput id="grant-repository-owner" [ngModel]="form().repository_owner"
               (ngModelChange)="patchForm({ repository_owner: $event })" />
      </gr-form-field>
      <gr-form-field label="Repository"
                     [hint]="form().repositoryGrantRepositorySelection === 'all' ? 'Project tasks in all repositories of the owner can use this connection. Repositories that are added later can use this connection too.' : ''">
        <gr-tab-switch ariaLabel="Repository" [options]="addGrantRepositoryOrAllRepositoriesTabSwitchOptions" [ngModel]="form().repositoryGrantRepositorySelection"
                       (ngModelChange)="patchForm({ repositoryGrantRepositorySelection: $event })" />
      </gr-form-field>
      @if (form().repositoryGrantRepositorySelection === 'one') {
        <gr-form-field label="Repository name" for="grant-repository-name" required
                       [invalid]="!!repositoryNameValidationError()" [error]="repositoryNameValidationError() ?? ''">
          <input grInput id="grant-repository-name" [ngModel]="form().repository_name"
                 (ngModelChange)="patchForm({ repository_name: $event })" />
        </gr-form-field>
      }
      <div grDialogFooter>
        <button grButton [label]="form().repositoryGrantRepositorySelection === 'all' ? 'Add grant to all repositories' : 'Add grant'"
                icon="check" (grSubmit)="sendAddGrantRequest()" [loading]="saving()"
                [disabled]="saving()" data-testid="save-repository-grant"></button>
      </div>
    </gr-dialog>
  `,
  styles: `
    @use '@gradient/ui/styles/variables' as *;

    :host { display: block; }
    .grants-head { display: flex; justify-content: flex-end; }
    .grants { list-style: none; margin: 0; padding: 0; }
    .grants li { display: flex; align-items: center; gap: $spacing-sm; padding: $spacing-xs 0; }
    .grant-repository { flex: 1; word-break: break-word; }
  `,
})
export class RepositoryGrantsComponent implements OnInit {
  private connections = inject(GitHostConnectionsApi);
  private projects = inject(ProjectsService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);

  gitHostConnectionOwner = input.required<GitHostConnectionOwner>();
  connectionId = input.required<string>();
  canAddAndRemoveGrants = input(false);

  protected readonly sourceLabel = getGrantSourceLabel;
  protected readonly repositoryText = repositoryTextForGrant;
  protected readonly addGrantRepositoryOrAllRepositoriesTabSwitchOptions = addGrantRepositoryOrAllRepositoriesTabSwitchOptions;

  connectionRepositoryGrants = signal<RepositoryGrantResponse[]>([]);
  loading = signal(true);
  loadError = signal<unknown>(null);

  projectOptionsForGrant = signal<SelectOption[]>([]);
  addRepositoryGrantDialogOpen = signal(false);
  saving = signal(false);
  addGrantFormApiErrorMessage = signal<string | null>(null);
  form = signal<AddRepositoryGrantForm>(emptyForm());

  projectFieldValidationError = computed(() => (this.form().project ? null : 'Select a project.'));
  repositoryOwnerValidationError = computed(() => {
    const owner = this.form().repository_owner.trim();
    if (!owner) return 'Enter a repository owner.';
    if (owner.length > repositoryOwnerOrNameMaxLength) return 'Enter 255 characters or fewer.';
    return isPlainPathSegment(owner) ? null : 'Use only letters, numbers, periods, underscores and hyphens.';
  });
  repositoryNameValidationError = computed(() => {
    if (this.form().repositoryGrantRepositorySelection === 'all') return null;
    const name = this.form().repository_name.trim();
    if (!name) return 'Enter a repository name or select all repositories of the owner.';
    if (name.length > repositoryOwnerOrNameMaxLength) return 'Enter 255 characters or fewer.';
    if (!name.split('/').every(isPlainPathSegment)) {
      return 'Join path segments with slashes. Path segments consist of letters, digits, dots, underscore characters and dashes.';
    }
    return name.endsWith('.git') ? 'Remove the trailing `.git`.' : null;
  });

  ngOnInit(): void {
    this.load();
  }

  load(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.connections.getGitHostConnectionRepositoryGrants(this.gitHostConnectionOwner(), this.connectionId()).subscribe({
      next: (grants) => {
        this.connectionRepositoryGrants.set(grants);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  getGrantProjectLabel(grant: RepositoryGrantResponse): string {
    return grant.project_name ?? 'Project outside the team';
  }

  grantRemoveDisabledReason(grant: RepositoryGrantResponse): string | null {
    return grant.source === 'state_file'
      ? 'The state file is the source of this grant. The next state apply adds the grant again. Change the state file to remove the grant.'
      : null;
  }

  patchForm(change: Partial<AddRepositoryGrantForm>): void {
    this.form.update((form) => ({ ...form, ...change }));
  }

  resetGrantFormLoadProjectOptionsAndOpenAddGrantDialog(): void {
    this.form.set(emptyForm());
    this.addGrantFormApiErrorMessage.set(null);
    this.addRepositoryGrantDialogOpen.set(true);
    const owner = this.gitHostConnectionOwner();
    this.projects.getProjects(1, 100).subscribe({
      next: (paginated) =>
        this.projectOptionsForGrant.set(
          paginated.items
            .filter((project) => owner.type !== 'team' || project.owner_team?.name === owner.name)
            .map((project) => ({ label: project.display_name || project.name, value: project.name })),
        ),
      error: () => {},
    });
  }

  sendAddGrantRequest(): void {
    const { project, repository_owner, repositoryGrantRepositorySelection, repository_name } = this.form();
    if (this.saving() || !project || this.repositoryOwnerValidationError() || this.repositoryNameValidationError()) return;
    const body: CreateRepositoryGrantRequest = { project, repository_owner: repository_owner.trim() };
    if (repositoryGrantRepositorySelection === 'one') body.repository_name = repository_name.trim();
    this.saving.set(true);
    this.addGrantFormApiErrorMessage.set(null);
    this.connections.addRepositoryGrantToGitHostConnection(this.gitHostConnectionOwner(), this.connectionId(), body).subscribe({
      next: (grant) => {
        this.saving.set(false);
        this.addRepositoryGrantDialogOpen.set(false);
        this.connectionRepositoryGrants.update((grants) => [...grants.filter((existing) => existing.id !== grant.id), grant]);
        this.notify.success('Repository grant added');
      },
      error: (error: unknown) => {
        this.saving.set(false);
        this.addGrantFormApiErrorMessage.set(apiErrorMessage(error, 'Failed to add repository grant'));
      },
    });
  }

  removeGrantAfterConfirmation(grant: RepositoryGrantResponse): void {
    if (this.grantRemoveDisabledReason(grant)) return;
    this.confirm
      .confirm({
        title: 'Delete Repository Grant',
        target: `${this.getGrantProjectLabel(grant)}: ${repositoryTextForGrant(grant)}`,
        consequence: 'Tasks in this project can no longer use connections for this repository.',
        confirmLabel: 'Delete',
        failure: 'Failed to delete the repository grant',
        action: () =>
          this.connections.deleteRepositoryGrantOfGitHostConnection(this.gitHostConnectionOwner(), this.connectionId(), grant.id),
      })
      .subscribe(() => {
        this.connectionRepositoryGrants.update((grants) => grants.filter((existing) => existing.id !== grant.id));
        this.notify.success('Repository grant deleted');
      });
  }
}
