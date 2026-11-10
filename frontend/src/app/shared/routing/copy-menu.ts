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
      { label: 'CLI command', icon: 'terminal', command: () => this.copy(references.gradientCliCommand, 'CLI command copied') },
      { label: 'API URL', icon: 'api', command: () => this.copy(`${this.config.apiBaseUrl}/${references.relativeApiEndpointPath}`, 'API URL copied') },
      { label: 'Link', icon: 'link', command: () => this.copy(link, 'Link copied') },
    ];
  }

  private copy(text: string, success: string): void {
    const written = navigator.clipboard?.writeText(text) ?? Promise.reject(new Error('Clipboard unavailable'));
    written.then(
      () => this.notify.success(success),
      (error: unknown) => this.notify.failure("Couldn't copy to clipboard", error),
    );
  }
}
