# Janus Initial Baseline

Date: `2026-08-18`

## Product truth

Janus exposes Projects on the main page and Sessions inside each Project. The
fork-sync workflow must make every repository and repair conversation visible
through those existing surfaces.

## Runtime truth

The current Automation webhook accepts one PR reference, creates one Project and
one Session, and records an Automation operation. Project creation and session
creation are durable operations processed by the server workers.

## Compatibility

The existing owner, project, session, GitHub credential, webhook-secret, and
operation records must remain usable. The existing data volume is preserved.

## Known gap

The current webhook selects only the first PR in a payload, does not model a
multi-repository batch, and exposes a Session ID without a direct UI link.
