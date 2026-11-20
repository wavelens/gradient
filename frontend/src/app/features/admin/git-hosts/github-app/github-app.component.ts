/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  HostListener,
  OnInit,
  computed,
  effect,
  inject,
  signal,
  untracked,
  viewChild,
} from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { ActivatedRoute, Router, RouterLink } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { AdminService } from '@core/services/admin.service';
import { ConfigService } from '@core/services/config.service';
import { GitHostConnectionsApi } from '@core/services/git-host-connections.service';
import { NotifyService } from '@core/services/notify.service';
import {
  ButtonComponent,
  CopyFieldComponent,
  FormFieldComponent,
  InputDirective,
  LoadStateComponent,
  MessageBannerComponent,
  SettingsSectionComponent,
} from '@gradient/ui/ui';
import { Observable, Subscription, catchError, concatMap, defaultIfEmpty, filter, finalize, interval, map, of, startWith, switchMap, take } from 'rxjs';
import { LabelHelpComponent } from '@shared/ui';
import { ConfirmService } from '@shared/confirm';
import { links } from '@shared/routing';
import { GitHostConnectionListComponent } from '@shared/git-host-connections/git-host-connection-list.component';
import type { AccessState, GitHostConnectionOwner, GitHostConnectionSummary, GitHostConnectionTaskResponse, ManifestResult } from '@core/models';

const INSTANCE: GitHostConnectionOwner = { type: 'instance' };
const INSTALLATION_CHECK_INTERVAL_MILLISECONDS = 2000;
const INSTALLATION_CHECK_ATTEMPTS = 8;

type InstallationReturnState =
  | { state: 'waiting' }
  | { state: 'installed'; connection: GitHostConnectionSummary; tasks: GitHostConnectionTaskResponse[] }
  | { state: 'missing' }
  | { state: 'requested' };

@Component({
  selector: 'app-admin-github-app',
  standalone: true,
  imports: [
    LabelHelpComponent,
    RouterLink,
    FormsModule,
    ButtonComponent,
    InputDirective,
    LoadStateComponent,
    MessageBannerComponent,
    SettingsSectionComponent,
    CopyFieldComponent,
    FormFieldComponent,
    GitHostConnectionListComponent,
  ],
  templateUrl: './github-app.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './github-app.component.scss',
})
export class GithubAppComponent implements OnInit {
  private admin = inject(AdminService);
  private connections = inject(GitHostConnectionsApi);
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);
  private destroyRef = inject(DestroyRef);
  private config = inject(ConfigService);

  protected readonly links = links;
  protected readonly instanceOwner = INSTANCE;
  protected readonly superuserAccess: AccessState = { managed: false, canEdit: true, canTrigger: true };
  protected readonly githubAppConfigured = this.config.githubAppEnabled;
  protected readonly appPageUrl = this.config.githubAppPageUrl;
  protected readonly installUrl = this.config.githubAppInstallUrl;
  protected readonly appName = this.appPageUrl?.split('/').pop() ?? null;

  host = signal<string>('');
  organization = signal<string>('');
  advancedOpen = signal(false);
  replacing = signal(false);
  normalizedGithubEnterpriseHost = computed(() => this.host().trim().replace(/^https?:\/\//i, '').replace(/\/+$/, ''));
  githubHostInvalidCharacterErrorMessage = computed(() =>
    /^[A-Za-z0-9.-]*$/.test(this.normalizedGithubEnterpriseHost())
      ? ''
      : 'Use only letters, numbers, dots and dashes (for example, ghe.example.com).',
  );
  organizationErrorMessage = computed(() =>
    /^[A-Za-z0-9-]*$/.test(this.organization().trim()) ? '' : 'Use only letters, numbers and hyphens.',
  );
  showsCreateForm = computed(() => !this.githubAppConfigured || this.replacing());
  loading = signal(false);

  showResult = signal(false);
  credentials = signal<ManifestResult | null>(null);
  credentialsMissing = signal(false);
  credentialsLoading = signal(false);
  credentialsError = signal<unknown>(null);

  installationReturn = signal<InstallationReturnState | null>(null);
  protected installationsList = viewChild(GitHostConnectionListComponent);
  private installationCheck?: Subscription;
  private returnedInstallationId: number | null = null;

  private keysVisible = computed(() => this.showResult() && this.credentials() !== null);

  private readonly leaveWarning =
    "The server only shows the generated credentials once and there's no way to recover them.";

  constructor() {
    effect(() => {
      const returned = this.installationReturn();
      const list = this.installationsList();
      if (returned?.state === 'installed' && list) untracked(() => list.showConnection(returned.connection.id));
    });
  }

  ngOnInit(): void {
    const query = this.route.snapshot.queryParamMap;
    if (query.get('ready') === '1') {
      this.showResult.set(true);
      this.loadCredentials();
    }
    const installationId = Number(query.get('installation_id'));
    if (query.get('setup_action') === 'request') {
      this.installationReturn.set({ state: 'requested' });
    } else if (Number.isInteger(installationId) && installationId > 0) {
      this.returnedInstallationId = installationId;
      this.checkInstallation();
    }
  }

  checkInstallation(): void {
    const installationId = this.returnedInstallationId;
    if (installationId === null) return;
    this.installationReturn.set({ state: 'waiting' });
    this.installationCheck?.unsubscribe();
    this.installationCheck = interval(INSTALLATION_CHECK_INTERVAL_MILLISECONDS)
      .pipe(
        startWith(-1),
        take(INSTALLATION_CHECK_ATTEMPTS),
        concatMap(() => this.connections.list(INSTANCE).pipe(catchError(() => of<GitHostConnectionSummary[]>([])))),
        map((list) => list.find((connection) => connection.github_installation_id === installationId)),
        filter((connection): connection is GitHostConnectionSummary => !!connection),
        take(1),
        switchMap((connection) =>
          this.connections.listTasks(INSTANCE, connection.id).pipe(
            catchError(() => of<GitHostConnectionTaskResponse[]>([])),
            map((tasks): InstallationReturnState => ({ state: 'installed', connection, tasks })),
          ),
        ),
        defaultIfEmpty<InstallationReturnState, InstallationReturnState>({ state: 'missing' }),
        takeUntilDestroyed(this.destroyRef),
      )
      .subscribe((result) => this.installationReturn.set(result));
  }

  create(): void {
    if (this.loading()) return;
    this.loading.set(true);
    const host = this.normalizedGithubEnterpriseHost() || undefined;
    const organization = this.organization().trim() || undefined;
    this.admin
      .requestGithubAppManifest(host, organization)
      .pipe(finalize(() => this.loading.set(false)))
      .subscribe({
        next: (resp) => this.submitManifestForm(resp.post_url, resp.manifest),
        error: (err: unknown) => this.notify.failure('Setup for GitHub App did not start', err),
      });
  }

  replace(): void {
    this.confirm
      .confirm({
        title: 'Replace GitHub App',
        target: this.appName ?? 'GitHub App',
        consequence: 'Installations of the current App stop delivering webhook events after a switch to the new App on the CI server.',
        confirmLabel: 'Replace',
        severity: 'danger',
        failure: 'Failed to start replacing the GitHub App.',
        action: () => of(true),
      })
      .subscribe(() => this.replacing.set(true));
  }

  private submitManifestForm(action: string, manifest: unknown): void {
    const form = document.createElement('form');
    form.method = 'POST';
    form.action = action;
    form.target = '_self';
    const input = document.createElement('input');
    input.type = 'hidden';
    input.name = 'manifest';
    input.value = JSON.stringify(manifest);
    form.appendChild(input);
    document.body.appendChild(form);
    form.submit();
  }

  loadCredentials(): void {
    this.credentialsLoading.set(true);
    this.credentialsError.set(null);
    this.admin.fetchGithubAppCredentials().subscribe({
      next: (creds) => {
        this.credentials.set(creds);
        this.credentialsLoading.set(false);
      },
      error: (err) => {
        this.credentialsLoading.set(false);
        if (err?.status === 404) {
          this.credentialsMissing.set(true);
        } else {
          this.credentialsError.set(err);
        }
      },
    });
  }

  removeReadyQueryParamAndReturnToSetupForm(): void {
    this.credentialsMissing.set(false);
    this.showResult.set(false);
    void this.router.navigate([], { relativeTo: this.route, queryParams: { ready: null }, queryParamsHandling: 'merge', replaceUrl: true });
  }

  canDeactivate(): boolean | Observable<boolean> {
    if (!this.keysVisible()) return true;
    return this.confirm
      .confirm({
        title: 'Leave page',
        target: 'GitHub App credentials',
        consequence: this.leaveWarning,
        confirmLabel: 'Leave',
        severity: 'danger',
        failure: 'Failed to leave the page.',
        action: () => of(true),
      })
      .pipe(defaultIfEmpty(false));
  }

  @HostListener('window:beforeunload', ['$event'])
  onBeforeUnload(event: BeforeUnloadEvent): void {
    if (this.keysVisible()) {
      event.preventDefault();
      event.returnValue = this.leaveWarning;
    }
  }
}
