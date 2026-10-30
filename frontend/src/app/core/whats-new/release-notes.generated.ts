// Generated from docs/whats-new by scripts/whats-new.mts. Do not edit.
export interface WhatsNewPage {
  title: string;
  html: string;
  icon?: string;
  image?: { src: string; alt: string };
}

export interface ReleaseNotes {
  version: string;
  pages: readonly WhatsNewPage[];
}

export const RELEASE_NOTES: readonly ReleaseNotes[] = [
];
