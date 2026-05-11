/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  input,
} from '@angular/core';
import { AccessState } from '@core/models/access.model';
import { MessageBannerComponent } from '@shared/components/form/message-banner/message-banner.component';
import { AccessService } from './access.service';

@Component({
  selector: 'app-access-banner',
  standalone: true,
  imports: [MessageBannerComponent],
  templateUrl: './access-banner.component.html',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class AccessBannerComponent {
  private access = inject(AccessService);

  state = input.required<AccessState | null | undefined>({ alias: 'access' });

  kind = computed(() => {
    const s = this.state();
    return s ? this.access.bannerKind(s) : 'none';
  });

  message = computed(() => {
    const s = this.state();
    return s ? this.access.bannerMessage(s) : null;
  });
}
