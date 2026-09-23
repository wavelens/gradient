/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, DestroyRef, ElementRef, effect, inject, signal, viewChild } from '@angular/core';
import { takeUntilDestroyed, toObservable } from '@angular/core/rxjs-interop';
import { Router } from '@angular/router';
import {
  EMPTY,
  Observable,
  Subject,
  catchError,
  debounceTime,
  defer,
  distinctUntilChanged,
  finalize,
  map,
  of,
  startWith,
  switchMap,
} from 'rxjs';
import { IconComponent } from '@shared/ui';
import { SearchService } from '@core/services/search.service';
import { SearchHit } from '@core/models';
import { CommandPaletteService } from './command-palette.service';

const DEBOUNCE_MS = 150;

const KIND_LABEL: Record<SearchHit['kind'], string> = {
  project: 'Project',
  task: 'Task',
  cache: 'Cache',
  nar: 'NAR',
  commit: 'Commit',
};

function isTextField(target: EventTarget | null): boolean {
  return target instanceof HTMLElement && !!target.closest('input, textarea, select, [contenteditable]:not([contenteditable="false"])');
}

function stepOf(e: KeyboardEvent): number {
  const key = e.key.toLowerCase();
  if (e.key === 'ArrowDown' || (e.ctrlKey && key === 'j')) return 1;
  if (e.key === 'ArrowUp' || (e.ctrlKey && key === 'k')) return -1;
  return 0;
}

interface Result {
  query: string;
  hits: SearchHit[];
  failed: boolean;
}

@Component({
  selector: 'app-command-palette',
  standalone: true,
  imports: [IconComponent],
  templateUrl: './command-palette.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './command-palette.component.scss',
  host: { '(document:keydown)': 'onGlobalKey($event)' },
})
export class CommandPaletteComponent {
  protected palette = inject(CommandPaletteService);
  private search = inject(SearchService);
  private router = inject(Router);
  private typed$ = new Subject<string>();
  private queryInput = viewChild<ElementRef<HTMLInputElement>>('query');

  protected readonly kindLabel = KIND_LABEL;
  protected result = signal<Result>({ query: '', hits: [], failed: false });
  protected active = signal(0);

  constructor() {
    toObservable(this.palette.isOpen)
      .pipe(
        switchMap((open) => (open ? this.session() : EMPTY)),
        takeUntilDestroyed(),
      )
      .subscribe((result) => {
        this.result.set(result);
        this.active.set(0);
      });
    effect(() => this.queryInput()?.nativeElement.focus());
    inject(DestroyRef).onDestroy(() => this.palette.close());
  }

  onInput(value: string): void {
    this.typed$.next(value);
  }

  onGlobalKey(e: KeyboardEvent): void {
    if (e.key === '/' && !e.ctrlKey && !e.metaKey && !e.altKey && !this.palette.isOpen() && !isTextField(e.target)) {
      e.preventDefault();
      this.palette.open();
    } else if (e.key === 'Escape' && this.palette.isOpen()) {
      this.palette.close();
    }
  }

  onKey(e: KeyboardEvent): void {
    const hits = this.result().hits;
    if (!hits.length) return;
    const step = stepOf(e);
    if (step) {
      e.preventDefault();
      this.active.set((this.active() + step + hits.length) % hits.length);
    } else if (e.key === 'Enter') {
      e.preventDefault();
      this.go(hits[this.active()]);
    }
  }

  go(hit: SearchHit): void {
    this.palette.close();
    void this.router.navigateByUrl(hit.route);
  }

  /// One open palette: starred items at once, then debounced typing; switchMap drops stale answers.
  private session(): Observable<Result> {
    return defer(() => {
      const returnFocus = document.activeElement as HTMLElement | null;
      this.result.set({ query: '', hits: [], failed: false });
      return this.typed$.pipe(
        debounceTime(DEBOUNCE_MS),
        map((q) => q.trim()),
        startWith(''),
        distinctUntilChanged(),
        switchMap((query) => this.lookup(query)),
        finalize(() => returnFocus?.focus()),
      );
    });
  }

  private lookup(query: string): Observable<Result> {
    return this.search.search(query).pipe(
      map((hits) => ({ query, hits, failed: false })),
      catchError(() => of({ query, hits: [], failed: true })),
    );
  }
}
