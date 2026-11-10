/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, booleanAttribute, computed, input, output } from '@angular/core';
import { NgTemplateOutlet } from '@angular/common';
import { RouterLink, type UrlTree } from '@angular/router';
import { ButtonComponent, EmptyStateComponent, MessageBannerComponent } from '@gradient/ui/ui';
import type { WaitingReason } from '@core/models';
import { type WaitingAccess, explainWaiting } from '@shared/evaluation';

@Component({
  selector: 'gr-waiting-notice',
  standalone: true,
  imports: [NgTemplateOutlet, RouterLink, ButtonComponent, EmptyStateComponent, MessageBannerComponent],
  template: `
    @let e = explanation();
    @if (banner()) {
      <gr-message-banner type="warning" icon="pause_circle">
        @if (subjectLabel(); as label) {
          @if (subjectLink(); as link) { <a class="waiting-subject" [routerLink]="link">{{ label }}</a> } @else { <span class="waiting-subject">{{ label }}</span> }
        }
        <strong>{{ e.title }}</strong> {{ e.detail }}
        <ng-container *ngTemplateOutlet="details" />
      </gr-message-banner>
    } @else {
      <gr-empty-state flat icon="pause_circle" [title]="e.title" [message]="e.detail">
        <ng-container *ngTemplateOutlet="details" />
      </gr-empty-state>
    }
    <ng-template #details>
      @if (e.requirements.length) {
        <ul class="waiting-requirements">
          @for (requirement of e.requirements; track requirement) { <li>{{ requirement }}</li> }
        </ul>
      }
      @if (e.action || pendingJobLink()) {
        <div class="waiting-next">
          @if (e.action; as action) {
            @if (action.kind === 'link') {
              <a grButton size="small" [label]="action.label" [icon]="action.icon" [routerLink]="action.link"></a>
            } @else {
              <button grButton size="small" [label]="action.label" [icon]="action.icon"
                      [loading]="busy()" [disabled]="busy()" (click)="start.emit()"></button>
            }
          }
          @if (pendingJobLink(); as jobLink) {
            <a grButton size="small" severity="secondary" label="Show job" icon="work" [routerLink]="jobLink"></a>
          }
        </div>
      }
      @if (!e.action && e.hint) {
        <p class="waiting-next waiting-hint">{{ e.hint }}</p>
      }
    </ng-template>
  `,
  styleUrl: './waiting-notice.component.scss',
  host: { '[class.waiting-notice--banner]': 'banner()' },
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class WaitingNoticeComponent {
  reason = input<WaitingReason | null | undefined>(null);
  project = input.required<string>();
  access = input.required<WaitingAccess>();
  banner = input(false, { transform: booleanAttribute });
  subjectLabel = input<string | null>(null);
  subjectLink = input<UrlTree | null>(null);
  pendingJobLink = input<UrlTree | null>(null);
  busy = input(false, { transform: booleanAttribute });
  start = output<void>();

  protected explanation = computed(() => explainWaiting(this.reason(), this.project(), this.access()));
}
