# Invites and Subscription Approval

Nobody is added to a project or a cache without agreeing to it, and no project
gets to use somebody else's cache without that cache's admins agreeing to it.
Both flows work the same way: a pending record is created, the other side
decides, and only then does the real membership or subscription exist.

## Inviting a member

A project admin with `manageMembers` (or a cache admin with
`manageCacheMembers`) invites by username, exactly as before:

```bash
curl -X POST https://gradient.example.com/api/v1/projects/acme/users \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"user": "alice", "role": "Write"}'
```

The response is now `Invitation sent` rather than a completed membership. Until
Alice accepts she has no access at all: no read, no builds, nothing. The pending
row shows under **Members & Roles** as *Pending Invitations*, where an admin can
revoke it.

An invitation expires after **7 days**. One user can hold only one open
invitation per project or cache; inviting again while one is open returns `409`.

## Accepting

Invitees see everything waiting for them under **Settings, My Invites**
(`/settings/invites`), with the role they were offered, who invited them, and
when it lapses. Accept creates the membership; decline deletes the invitation.

If e-mail is configured the invitee also gets a message with a direct link. That
link is a shortcut, not an authorisation: accepting requires being signed in as
the invited account, so a forwarded mail is useless to anybody else. An expired
token is refused and discarded.

## Requesting a cache subscription

A project admin with `manageSubscriptions` subscribes to any cache they can see:

```bash
curl -X POST https://gradient.example.com/api/v1/projects/acme/subscribe/shared-cache \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"mode": 0}'
```

What happens next depends on the caller. Holding `manageCacheSubscriptions` on
that cache too, the subscription is created immediately, as it always was.
Without it the call records a **subscription request** and answers
`Subscription requested`. This is what makes cross-organisation subscription
possible at all: previously the endpoint demanded admin rights on both sides, so
only somebody who ran both could link them.

The project's **Cache Subscriptions** page lists the request with a *Pending
approval* badge. It grants nothing while it waits, and the same button that
unsubscribes an active cache cancels a pending request.

## Approving a request

Cache admins with `manageCacheSubscriptions` find waiting requests under the
cache's **Subscriptions** page (`/caches/<cache>/subscriptions`), showing which
project asked, in which mode, and who asked.

Approving creates the subscription with the mode that was requested and re-queues
any evaluation that was parked for want of a cache. Denying deletes the request;
the project is free to ask again.

## Without e-mail

Every notification is skipped when no SMTP server is configured
(`GRADIENT_EMAIL_*` unset). Nothing fails and nothing is logged as an error: the
invitation and request rows are still written, and the My Invites page and the
two pending lists carry the whole flow. Mail is an extra channel, never the
mechanism.

## Who skips invitations

Two cases bypass the flow entirely:

- **Superusers** already holding the permission on that project or cache add
  members directly instead of inviting. This is not a blanket power: superuser is
  not an implicit grant over projects they have no role in, so the bypass only
  applies where they could have invited anyway.
- **State-managed** projects and caches are provisioned from the state file. The
  member endpoints reject them outright, and `gradient-state` writes memberships
  and subscriptions straight to the database, so nothing there ever waits on an
  invitation.
