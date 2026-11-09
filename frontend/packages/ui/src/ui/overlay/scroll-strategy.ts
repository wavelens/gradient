/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ScrollStrategy } from '@angular/cdk/overlay';

/// Lock scroll without moving the page. Block strategies in CDK scroll back to the old offset on dialog close. Dialog actions navigate to another page and that scroll back cancels the scroll to top.
export class HtmlOverflowHiddenScrollStrategy implements ScrollStrategy {
  private previousOverflow: string | null = null;

  constructor(private readonly root: HTMLElement = document.documentElement) {}

  attach(): void {}

  enable(): void {
    if (this.previousOverflow !== null) return;
    this.previousOverflow = this.root.style.overflow;
    this.root.style.overflow = 'hidden';
  }

  disable(): void {
    if (this.previousOverflow === null) return;
    this.root.style.overflow = this.previousOverflow;
    this.previousOverflow = null;
  }

  detach(): void {
    this.disable();
  }
}
