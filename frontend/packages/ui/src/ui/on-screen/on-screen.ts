/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { DestroyRef, ElementRef, Signal, WritableSignal, inject, signal } from '@angular/core';

const watched = new WeakMap<Element, WritableSignal<boolean>>();
let observer: IntersectionObserver | undefined;

function sharedObserver(): IntersectionObserver | undefined {
  if (typeof IntersectionObserver === 'undefined') return undefined;
  observer ??= new IntersectionObserver((entries) => {
    for (const entry of entries) watched.get(entry.target)?.set(entry.isIntersecting);
  });

  return observer;
}

/// Whether the host element is on screen. An endless animation that nobody sees still costs a
/// frame of the whole page every time it ticks, so a component stops it while this is false.
export function injectOnScreen(): Signal<boolean> {
  const host = inject<ElementRef<Element>>(ElementRef).nativeElement;
  const onScreen = signal(true);
  const shared = sharedObserver();
  if (!shared) return onScreen;
  watched.set(host, onScreen);
  shared.observe(host);
  inject(DestroyRef).onDestroy(() => shared.unobserve(host));

  return onScreen;
}
