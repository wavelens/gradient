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
  { version: "2.1", pages: [{"title":"Restricted Evaluation","html":"<p>Evaluations use Nix <code>restrict-eval</code>. Fetches during evaluation can reach only inputs locked by <code>flake.lock</code> and allowed URL prefixes.</p><ul><li>Existing projects have restrictions after an upgrade.</li><li>Add URL prefixes per project in project settings, or server-wide with <code>GRADIENT_EVAL_ALLOWED_URI_PREFIXES</code>.</li><li>Clear, in project settings, <strong>Restrict fetches during evaluation to locked flake inputs and allowed URL prefixes</strong> to turn off per-project restrictions.</li></ul>","icon":"lock"}] },
  { version: "2.0", pages: [{"title":"A Fresh Dashboard","html":"<p>Starred projects and caches remain in the sidebar. Both update right away.</p>","image":{"src":"whats-new/2.0/dashboard.png","alt":"Dashboard"}}] },
];
