/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, inject } from '@angular/core';
import { ButtonComponent, DialogComponent } from '@gradient/ui/ui';
import { WhatsNewService } from './whats-new.service';

@Component({
  selector: 'app-whats-new-dialog',
  standalone: true,
  imports: [ButtonComponent, DialogComponent],
  template: `
    <gr-dialog header="What's New!" [visible]="whatsNew.open()" (visibleChange)="!$event && whatsNew.close()" width="560px">
      @for (release of whatsNew.notes(); track release.version) {
        <section class="release">
          <h3>Gradient {{ release.version }}</h3>
          <div class="notes" [innerHTML]="release.html"></div>
        </section>
      }
      <div grDialogFooter>
        <button grButton label="Got it" (click)="whatsNew.close()"></button>
      </div>
    </gr-dialog>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './whats-new-dialog.component.scss',
})
export class WhatsNewDialogComponent {
  protected whatsNew = inject(WhatsNewService);
}
