 - Base (Default) Workers (for all orgs, default deactivated), configured only in state
 - when new git commit is detected, add it as queued (dont wait for the current evaluation to finish), abort the previous build if it is still running
 - evaluation queued should send pending to reporter (currently when starting evaluation)

 - no error message and recommendation when adding internal cache
 - entry point metrics for builds that are completed but the eval is still running wont show up.
 - "Created" state has no colored dot in the log page
 - the builds text name should cutoff with ... when to long for the sidebar

