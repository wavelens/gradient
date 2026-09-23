/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import type { StatusPhase } from '@shared/evaluation';
import { StatusIconComponent } from './status-icon.component';

interface FakeSpin {
  playState: AnimationPlayState;
  updatePlaybackRate: ReturnType<typeof vi.fn>;
  cancel: ReturnType<typeof vi.fn>;
  effect: { getComputedTiming: () => { currentIteration: number | null }; updateTiming: ReturnType<typeof vi.fn> };
}

function fakeSpin(): FakeSpin {
  return {
    playState: 'running',
    updatePlaybackRate: vi.fn(),
    cancel: vi.fn(),
    effect: { getComputedTiming: () => ({ currentIteration: 2 }), updateTiming: vi.fn() },
  };
}

function render(phase: StatusPhase, label?: string): ComponentFixture<StatusIconComponent> {
  const fixture = TestBed.createComponent(StatusIconComponent);
  fixture.componentRef.setInput('phase', phase);
  if (label) fixture.componentRef.setInput('label', label);
  fixture.detectChanges();
  return fixture;
}

function change(fixture: ComponentFixture<StatusIconComponent>, phase: StatusPhase): void {
  fixture.componentRef.setInput('phase', phase);
  fixture.detectChanges();
}

const host = (f: ComponentFixture<StatusIconComponent>): HTMLElement => f.nativeElement;

describe('StatusIconComponent', () => {
  const originalAnimate = Element.prototype.animate;
  const originalMatchMedia = window.matchMedia;
  let spin: FakeSpin;
  let animate: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    spin = fakeSpin();
    animate = vi.fn(() => spin);
    Element.prototype.animate = animate as unknown as typeof Element.prototype.animate;
  });

  afterEach(() => {
    Element.prototype.animate = originalAnimate;
    window.matchMedia = originalMatchMedia;
  });

  it('reflects the phase on the host and follows changes', () => {
    const fixture = render('queued');
    expect(host(fixture).dataset['phase']).toBe('queued');
    change(fixture, 'failure');
    expect(host(fixture).dataset['phase']).toBe('failure');
  });

  it('renders the first phase statically', () => {
    const fixture = render('success');
    expect(host(fixture).hasAttribute('data-animate')).toBe(false);
  });

  it('animates once the phase changes', () => {
    const fixture = render('running');
    change(fixture, 'success');
    expect(host(fixture).hasAttribute('data-animate')).toBe(true);
  });

  it('stays static when the same phase is set again', () => {
    const fixture = render('success');
    change(fixture, 'success');
    expect(host(fixture).hasAttribute('data-animate')).toBe(false);
  });

  it('is decorative without a label', () => {
    const fixture = render('success');
    expect(host(fixture).getAttribute('aria-hidden')).toBe('true');
    expect(host(fixture).getAttribute('role')).toBeNull();
  });

  it('exposes a label to assistive tech', () => {
    const fixture = render('failure', 'Failed');
    expect(host(fixture).getAttribute('role')).toBe('img');
    expect(host(fixture).getAttribute('aria-label')).toBe('Failed');
    expect(host(fixture).getAttribute('aria-hidden')).toBeNull();
  });

  it('starts no spin for a still phase', () => {
    render('success');
    expect(animate).not.toHaveBeenCalled();
  });

  it('spins while running at full rate and queued at a quarter', () => {
    const fixture = render('queued');
    expect(animate).toHaveBeenCalledTimes(1);
    expect(spin.updatePlaybackRate).toHaveBeenLastCalledWith(0.25);
    change(fixture, 'running');
    expect(animate).toHaveBeenCalledTimes(1);
    expect(spin.updatePlaybackRate).toHaveBeenLastCalledWith(1);
  });

  it('finishes the current lap instead of snapping when the run ends', () => {
    const fixture = render('running');
    change(fixture, 'success');
    expect(spin.effect.updateTiming).toHaveBeenLastCalledWith({ iterations: 3 });
    expect(spin.cancel).not.toHaveBeenCalled();
  });

  it('resumes the same spin when a finished run restarts', () => {
    const fixture = render('running');
    change(fixture, 'success');
    change(fixture, 'running');
    expect(animate).toHaveBeenCalledTimes(1);
    expect(spin.effect.updateTiming).toHaveBeenLastCalledWith({ iterations: Infinity });
  });

  it('cancels the spin on destroy', () => {
    const fixture = render('running');
    fixture.destroy();
    expect(spin.cancel).toHaveBeenCalled();
  });

  it('never moves under reduced motion', () => {
    window.matchMedia = vi.fn(() => ({ matches: true })) as unknown as typeof window.matchMedia;
    const fixture = render('running');
    change(fixture, 'success');
    expect(animate).not.toHaveBeenCalled();
    expect(host(fixture).hasAttribute('data-animate')).toBe(false);
  });

  it('stays static where the browser has no Web Animations', () => {
    Element.prototype.animate = undefined as unknown as typeof Element.prototype.animate;
    const fixture = render('running');
    change(fixture, 'success');
    expect(host(fixture).hasAttribute('data-animate')).toBe(false);
  });
});
