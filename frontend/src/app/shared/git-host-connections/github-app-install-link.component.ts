/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, inject, input } from '@angular/core';
import { ButtonComponent } from '@gradient/ui/ui';
import { ConfigService } from '@core/services/config.service';

@Component({
  selector: 'app-github-app-install-link',
  standalone: true,
  imports: [ButtonComponent],
  template: `
    @if (installUrl) {
      <a grButton label="Install on GitHub" icon="add" [size]="size()" [severity]="primary() ? 'primary' : 'secondary'"
        [href]="installUrl" data-testid="github-app-install"></a>
    }
  `,
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class GithubAppInstallLinkComponent {
  protected readonly installUrl = inject(ConfigService).githubAppInstallUrl;
  primary = input(false);
  size = input<'small' | 'normal'>('normal');
}
