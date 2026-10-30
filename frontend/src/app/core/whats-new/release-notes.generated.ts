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
  { version: "2.0", pages: [{"title":"A Fresh Dashboard","html":"<p>Starred projects and caches remain in the sidebar. Both update right away.</p>","image":{"src":"whats-new/2.0/dashboard.png","alt":"Dashboard"}}] },
];
