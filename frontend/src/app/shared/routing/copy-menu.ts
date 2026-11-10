/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Router, UrlTree } from '@angular/router';
import type { MenuItem } from '@gradient/ui/ui';
import { ConfigService } from '@core/services/config.service';
import { NotifyService } from '@core/services/notify.service';

export interface CopyMenuItemCommandApiPathAndWebPageRoute {
  gradientCliCommand: string;
  relativeApiEndpointPath: string;
  absoluteLinkUrlTree: UrlTree;
}

@Injectable({ providedIn: 'root' })
export class CopyMenuService {
  private config = inject(ConfigService);
  private notify = inject(NotifyService);
  private router = inject(Router);

  createCopyMenuItemWithCliCommandApiUrlAndLink(references: CopyMenuItemCommandApiPathAndWebPageRoute): MenuItem {
    return { label: 'Copy', icon: 'content_copy', submenuItems: this.getFlatCopyEntries(references) };
  }

  getFlatCopyEntries(references: CopyMenuItemCommandApiPathAndWebPageRoute): MenuItem[] {
    const link = `${globalThis.location?.origin ?? ''}${this.router.serializeUrl(references.absoluteLinkUrlTree)}`;
    return [
      { label: 'CLI command', icon: 'terminal', command: () => this.notify.copyToClipboard(references.gradientCliCommand, 'CLI command') },
      { label: 'API URL', icon: 'api', command: () => this.notify.copyToClipboard(`${this.config.apiBaseUrl}/${references.relativeApiEndpointPath}`, 'API URL') },
      { label: 'Link', icon: 'link', command: () => this.notify.copyToClipboard(link, 'link') },
    ];
  }
}
