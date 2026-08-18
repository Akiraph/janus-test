# Fork-sync Automation Design

## Goal

Integrate fork-sync reports into Janus so one signed webhook can process many
repositories serially. Each repository is represented by a reusable Project;
each repair attempt is represented by a new Session visible in that Project.
The AI may commit and push the repair directly to the repository default branch
when an explicitly Automation-enabled GitHub credential is available.

## Architecture

The HTTP webhook is the external contract owner. It validates the secret and
normalizes a list of repository work items. A durable batch operation owns
ordering and resumes one child operation at a time. A child operation owns one
repository: it reuses or creates the Project, creates a Session, runs the AI
repair, and records push/result state. Projects remain the canonical repository
identity owner; Sessions remain the canonical conversation owner.

## Webhook contract

Preferred JSON body:

```json
{
  "workflow": "fork-sync",
  "source": "happy-tts",
  "repositories": [
    {
      "pull_request_url": "https://github.com/owner/repo/pull/42",
      "repository_url": "https://github.com/owner/repo.git",
      "branch": "main",
      "project_name": "owner/repo"
    }
  ]
}
```

The existing raw HTML and single-PR JSON envelopes remain compatible and map to
one repository item. Duplicate delivery is idempotent per batch and per
repository item.

## Project and Session rules

- Canonical Project key: owner plus normalized GitHub repository URL; `.git`,
  scheme, host aliases, and trailing slash differences do not create duplicates.
- Existing ready Project: reuse its workspace and metadata.
- Existing creating Project: wait for its create operation.
- Existing error Project: use the existing retry/recovery path; never delete it.
- Every non-duplicate repository item creates a new Session, preserving prior
  repair conversations as history.
- The main Project list, Project workspace, Sessions panel, and Automation run
  record all refer to the same IDs.

## AI and push behavior

The Session prompt requires inspecting the current workspace, preserving
uncommitted work, checking the repository default branch, resolving the
reported conflict, testing, committing, and pushing to that default branch.
The AI must not run `git reset --hard`, `git clean`, or discard unrelated local
changes. If unrelated changes make a safe push impossible, the child becomes
`needs_attention` with the Project and Session still visible.

An explicitly Automation-enabled encrypted PAT is required. The workflow does
not silently fall back to read-only mode. Credentials remain project-scoped and
are injected only into the Session tool environment as `GH_TOKEN` and
`GITHUB_TOKEN`.

## UI

Automation shows one batch and its ordered child rows. Each child displays the
repository, current step, push state, Project link, Session link, and PR link.
The normal Project list and workspace remain the source of truth for navigation.

## Verification

- Rust tests cover multi-item parsing, canonical Project reuse, serial child
  ordering, idempotency, missing push credentials, and uncommitted-work safety.
- Frontend tests cover Project/Session links and batch progress rendering.
- Run `cargo fmt --all -- --check`, targeted server tests, frontend typecheck,
  frontend build, and the existing session/webhook tests.
- Deploy with existing migrations and volume unchanged; verify both containers
  healthy and the public bundle contains the new UI.

## Non-goals

This design does not move fork scanning into Janus, replace the friend script's
GitHub API logic, or auto-delete old Projects/Sessions. The script remains the
producer of the repository list; Janus owns serial repair execution and
visibility.

## Decision

Use a durable parent batch plus serial child operations. A single looping
operation would make recovery and per-repository UI state opaque; one webhook
per repository would leave the required batching and ordering outside Janus.
