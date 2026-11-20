/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, Injector, OnInit, afterNextRender, computed, inject, input, signal, viewChild } from '@angular/core';
import { ActivatedRoute, RouterLink } from '@angular/router';
import {
  BadgeComponent,
  ButtonComponent,
  CopyFieldComponent,
  EmptyStateComponent,
  LoadStateComponent,
  MenuComponent,
  type MenuItem,
} from '@gradient/ui/ui';
import {
  AccessState,
  GitHostConnectionOwner,
  GitHostConnectionSummary,
  GitHostConnectionTaskResponse,
  GitHostType,
  getGitHostConnectionOwnerReadableText,
  isSameGitHostConnectionOwner,
} from '@core/models';
import { GitHostConnectionsApi } from '@core/services/git-host-connections.service';
import { NotifyService } from '@core/services/notify.service';
import { ConfirmService } from '@shared/confirm';
import { AccessService } from '@shared/access/access.service';
import { IncomingDeliveriesComponent, deliveryBadgeSeverity, deliverySummary, gitHostLabel } from '@shared/git';
import { links } from '@shared/routing';
import { CurrentTimeSignal, RelativeTimePipe } from '@shared/text';
import { formatEntityLabel } from '@shared/ui';
import { GitHostConnectionDialogComponent } from './git-host-connection-dialog.component';
import { accessTokenState, gitHostConnectionHost, refusedSignatureNewerThanLastDelivery } from './git-host-connection-health';
import { GitHostOAuthReturnBannerComponent } from './git-host-oauth-return-banner.component';

@Component({
  selector: 'app-git-host-connection-list',
  standalone: true,
  imports: [
    RouterLink,
    BadgeComponent,
    ButtonComponent,
    CopyFieldComponent,
    EmptyStateComponent,
    LoadStateComponent,
    MenuComponent,
    RelativeTimePipe,
    IncomingDeliveriesComponent,
    GitHostConnectionDialogComponent,
    GitHostOAuthReturnBannerComponent,
  ],
  templateUrl: './git-host-connection-list.component.html',
  styleUrl: './git-host-connection-list.component.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class GitHostConnectionListComponent implements OnInit {
  private api = inject(GitHostConnectionsApi);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);
  private route = inject(ActivatedRoute);
  private injector = inject(Injector);
  private clock = inject(CurrentTimeSignal);
  protected accessService = inject(AccessService);
  private dialog = viewChild.required(GitHostConnectionDialogComponent);

  gitHostConnectionOwner = input.required<GitHostConnectionOwner>();
  access = input.required<AccessState>();
  gitHostTypes = input<readonly GitHostType[] | null>(null);
  showsEmptyState = input(true);
  isNewConnectionButtonPrimary = input(true);

  protected readonly gitHostLabel = gitHostLabel;
  protected readonly ownerText = getGitHostConnectionOwnerReadableText;
  protected readonly deliverySummary = deliverySummary;
  protected readonly refusedSignatureNewerThanLastDelivery = refusedSignatureNewerThanLastDelivery;
  protected readonly deliveryBadgeSeverity = deliveryBadgeSeverity;
  protected readonly host = gitHostConnectionHost;
  protected readonly links = links;

  connections = signal<GitHostConnectionSummary[]>([]);
  loading = signal(true);
  loadError = signal<unknown>(null);
  deliveriesShownFor = signal<string | null>(null);
  tasksShownFor = signal<string | null>(null);
  tasks = signal<GitHostConnectionTaskResponse[] | null>(null);
  testingId = signal<string | null>(null);
  linkedId = signal<string | null>(null);
  menuItems = signal<MenuItem[]>([]);

  writable = computed(() => this.accessService.isWritable(this.access()));
  private shownConnections = computed(() => {
    const types = this.gitHostTypes();
    return types ? this.connections().filter((c) => types.includes(c.git_host_type as GitHostType)) : this.connections();
  });
  ownConnections = computed(() => this.shownConnections().filter((c) => this.isOwn(c)));
  ownConnectionNames = computed(() => this.connections().filter((c) => this.isOwn(c)).map((c) => c.name));
  sortedConnections = computed(() => [...this.ownConnections(), ...this.shownConnections().filter((c) => !this.isOwn(c))]);

  ngOnInit(): void {
    this.linkedId.set(this.route.snapshot.queryParamMap.get('connection'));
    this.load();
  }

  load(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.api.list(this.gitHostConnectionOwner()).subscribe({
      next: (list) => {
        this.connections.set(list);
        this.loading.set(false);
        this.scrollToLinkedConnection();
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  showConnection(id: string): void {
    this.linkedId.set(id);
    this.load();
  }

  isOwn(connection: GitHostConnectionSummary): boolean {
    return isSameGitHostConnectionOwner(connection.owner, this.gitHostConnectionOwner());
  }

  canManageRow(connection: GitHostConnectionSummary): boolean {
    return this.isOwn(connection) && (connection.can_manage ?? false) && this.writable();
  }

  canEditRow(connection: GitHostConnectionSummary): boolean {
    return this.canManageRow(connection) && !connection.managed && connection.git_host_type !== 'github';
  }

  canReadDeliveries(connection: GitHostConnectionSummary): boolean {
    return connection.receives_events && (connection.can_manage || connection.owner.type === 'project');
  }

  tokenState(connection: GitHostConnectionSummary) {
    return accessTokenState(connection, this.clock.currentTimeMilliseconds());
  }

  openCreate(): void {
    this.dialog().openCreate();
  }

  openEdit(connection: GitHostConnectionSummary): void {
    this.dialog().openEdit(connection);
  }

  toggleDeliveries(connection: GitHostConnectionSummary): void {
    this.deliveriesShownFor.update((shown) => (shown === connection.id ? null : connection.id));
  }

  toggleTasks(connection: GitHostConnectionSummary): void {
    if (this.tasksShownFor() === connection.id) {
      this.tasksShownFor.set(null);
      return;
    }
    this.tasksShownFor.set(connection.id);
    this.tasks.set(null);
    this.api.listTasks(this.gitHostConnectionOwner(), connection.id).subscribe({
      next: (tasks) => {
        if (this.tasksShownFor() === connection.id) this.tasks.set(tasks);
      },
      error: (error: unknown) => {
        this.tasksShownFor.set(null);
        this.notify.failure('Failed to load connection tasks', error);
      },
    });
  }

  test(connection: GitHostConnectionSummary): void {
    if (this.testingId()) return;
    this.testingId.set(connection.id);
    this.api.test(this.gitHostConnectionOwner(), connection.id).subscribe({
      next: (result) => {
        this.testingId.set(null);
        if (result.success) this.notify.success(`${connection.display_name}: Signed in as ${result.account ?? 'an unknown account'}`);
        else this.notify.failure(`${connection.display_name}: ${result.error ?? 'the Git host refused the access token.'}`, null);
      },
      error: (error: unknown) => {
        this.testingId.set(null);
        this.notify.failure(`Couldn't test ${connection.display_name}`, error);
      },
    });
  }

  webhookUrl(connection: GitHostConnectionSummary): string | null {
    return connection.receives_events && connection.git_host_type !== 'github' ? (connection.webhook_url ?? null) : null;
  }

  openMenu(event: Event, connection: GitHostConnectionSummary, menu: MenuComponent): void {
    const url = this.webhookUrl(connection);
    const items: MenuItem[] = url
      ? [{ label: 'Copy webhook URL', icon: 'content_copy', command: () => void this.notify.copyToClipboard(url, 'webhook URL') }]
      : [];
    if (this.canManageRow(connection)) {
      items.push({ label: 'Test', icon: 'play_arrow', command: () => this.test(connection) });
      if (!connection.managed) {
        items.push({ label: 'Delete', icon: 'delete', danger: true, command: () => this.remove(connection) });
      }
    }
    this.menuItems.set(items);
    menu.toggle(event);
  }

  remove(connection: GitHostConnectionSummary): void {
    this.confirm
      .confirm({
        title: 'Delete git host connection',
        target: formatEntityLabel(connection),
        consequence: 'Tasks that use this connection lose it and stop reporting commit status.',
        isIrreversible: true,
        confirmLabel: 'Delete',
        failure: "Couldn't delete git host connection",
        action: () => this.api.delete(this.gitHostConnectionOwner(), connection.id),
      })
      .subscribe(() => {
        this.connections.update((list) => list.filter((c) => c.id !== connection.id));
        this.notify.success('Git host connection deleted');
      });
  }

  private scrollToLinkedConnection(): void {
    const id = this.linkedId();
    if (!id) return;
    afterNextRender(() => document.getElementById(`connection-${id}`)?.scrollIntoView({ block: 'center' }), { injector: this.injector });
  }
}
