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
  { version: "2.1", pages: [{"title":"UX Improvements","html":"<p>Navigation is faster and easier to read.</p><ul><li>Live Jobs show running jobs with project, task and worker.</li><li>Press <code>Ctrl+K</code> or <code>/</code> to open the command palette and jump to a project, task, or cache.</li><li>Build logs have an attempt selector, with a separate log for each attempt.</li><li>The <strong>Readiness</strong> card on a project page can show when Workers, a writable cache, or Git access are missing.</li><li>The separate access pages give way to one member page.</li></ul>","icon":"auto_awesome"},{"title":"Gradient Exec","html":"<p>Run programs in a virtual machine on a worker, outside the Nix sandbox.</p><ul><li>Turn exec on or off for each task.</li><li>Choose the guest size and work disk. Prepare pre-warmed guests ahead of time.</li><li>Start the exec job automatically after the program build is complete. Distribute guests across a multi-node cluster.</li><li>Use custom exec images.</li><li>Use <code>gradient run</code> to start a program. Use <code>-L</code> to forward a local port.</li></ul>","icon":"terminal"},{"title":"Multi-Node Evaluations","html":"<p>Tasks that enable multi-node evaluations split large evaluations across workers.</p><p>Cost estimates drive the scheduler to split an evaluation into shards. Workers evaluate shards in parallel, so large flakes finish faster.</p>","icon":"hub"},{"title":"SBOMs","html":"<p>Every successful build has a software bill of materials (SBOM).</p><ul><li>Download the document as CycloneDX JSON from the <strong>SBOM</strong> entry in the build menu.</li><li>The API can serve the same document.</li></ul>","icon":"inventory_2"},{"title":"Admin Area","html":"<p>Server administrators can access the Admin Area.</p><ul><li>Tabs for <strong>Overview</strong>, <strong>Workers</strong>, <strong>Storage</strong>, <strong>Git hosts</strong>, and <strong>Settings</strong>.</li><li>Use <strong>Enable Draining</strong> to stop new evaluations and builds from starting on the whole instance. Use <strong>Drain</strong> on the Workers tab to stop one worker from taking new work.</li><li>Use <strong>Drain All</strong> on the Workers tab to stop all workers from taking new work. Use <strong>Undrain All</strong> to end the drain.</li><li>Use <strong>Run Deep GC</strong> to delete stored files that no record in the database references.</li></ul>","icon":"admin_panel_settings"},{"title":"Pinned Cache Paths","html":"<p>Keep packages in the cache through garbage collection.</p><ul><li>Pin or unpin a package in the list of packages in the cache.</li><li>Pin everything that the package references when needed and keep release artifacts installable.</li><li>Filter the list by pin status.</li></ul>","icon":"push_pin"}] },
  { version: "2.0", pages: [{"title":"A Fresh Dashboard","html":"<p>Starred projects and caches remain in the sidebar. Both update right away.</p>","image":{"src":"whats-new/2.0/dashboard.png","alt":"Dashboard"}}] },
];
