/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, inject, input, output } from '@angular/core';
import { ButtonComponent, ButtonSeverity } from '@gradient/ui/ui';
import { AuthService } from '@core/services/auth.service';
import { AccessService, accessDisabledReason } from '@shared/access';
import type { AccessState } from '@core/models';

const CACHE_LIMIT_REACHED = 'This account reached the maximum number of caches. Ask an administrator to raise the limit.';

@Component({
  selector: 'app-create-cache-button',
  standalone: true,
  imports: [ButtonComponent],
  changeDetection: ChangeDetectionStrategy.Eager,
  template: `
    <button
      grButton
      label="Create Cache"
      icon="add"
      [severity]="severity()"
      [disabledReason]="disabledReason()"
      (click)="create.emit()"
    ></button>
  `,
})
export class CreateCacheButtonComponent {
  private auth = inject(AuthService);
  private accessService = inject(AccessService);

  severity = input<ButtonSeverity>('primary');
  access = input<AccessState | null>(null);
  create = output();

  protected disabledReason = computed(() => {
    const access = this.access();
    if (access && this.accessService.shouldDisableInput(access)) return accessDisabledReason(access);
    if (this.auth.user()?.can_create_cache === false) return CACHE_LIMIT_REACHED;
    return '';
  });
}
