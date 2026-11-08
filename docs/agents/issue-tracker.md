# Issue tracker

Issues and specs live in GitHub Issues for `Mrsmithz/mcp-bitbucket`.
Use the `gh` CLI from this repository.

## Conventions

- Create an issue with `gh issue create --title "..." --body "..."`.
  Use a heredoc for multi-line bodies.
- Read an issue with `gh issue view <number> --json number,title,body,labels,comments`.
- List issues with `gh issue list --state open --json number,title,body,labels,comments`.
  Apply label and state filters as needed.
- Comment with `gh issue comment <number> --body "..."`.
- Apply labels with `gh issue edit <number> --add-label "..."`.
- Remove labels with `gh issue edit <number> --remove-label "..."`.
- Before closing an issue, post the explanation in a separate comment.
  Then run `gh issue close <number>`.
  On an already-closed issue, `gh issue close --comment` drops the comment
  even though the command exits successfully.

Before the first issue creation in a session, run
`gh repo view --json nameWithOwner`.
The target must be `Mrsmithz/mcp-bitbucket`.
If it differs, confirm the target with the user.

File issues only in the user's repository.
Never file them in `mattpocock/skills` unless the user owns it.

## Sub-issues and dependencies

With gh 2.94 or newer:

- Create a sub-issue with `gh issue create --parent <parent> ...`.
- Link an existing child with
  `gh issue edit <parent> --add-sub-issue <child>`.
- Create an issue with blockers using
  `gh issue create --blocked-by <number>,<number> ...`.
- Add a blocker with
  `gh issue edit <child> --add-blocked-by <blocker>`.

Prefer GitHub's native sub-issues and dependencies.
If unavailable, put `Part of #<parent>` or
`Blocked by: #<number>, #<number>` at the top of the child body.
For the sub-issue fallback, also add the child to the parent's task list.

An issue is unblocked when every blocker is closed.
Count open native blockers with:
`gh api repos/Mrsmithz/mcp-bitbucket/issues/<number> --jq .issue_dependencies_summary.blocked_by`.

## Pull requests as a triage surface

**PRs as a request surface: no.**

If changed to `yes`, use the same labels and states for external PRs.

- Read the body with `gh pr view <number>`.
- Read comments separately with `gh pr view <number> --comments`.
- Read the diff with `gh pr diff <number>`.
- Comment, label, and close with the corresponding `gh pr` commands.
- List external open PRs with:
  `gh api --paginate 'repos/Mrsmithz/mcp-bitbucket/pulls?state=open' --jq '.[] | select(.author_association | IN("OWNER","MEMBER","COLLABORATOR") | not) | {number, title, author: .user.login, author_association, labels: [.labels[].name]}'`.

Issues and PRs share one number space.
Resolve an ambiguous number with `gh pr view <number>`, then fall back
to `gh issue view <number>`.

## Skill instructions

When a skill says "publish to the issue tracker", create a GitHub issue.

When a skill says "fetch the relevant ticket", read the GitHub issue,
including its labels and comments.

## Wayfinding

The map is one issue labelled `wayfinder:map`.
Its body holds Notes, Decisions-so-far, and Fog.
Link child tickets as sub-issues, using the fallback above if needed.

Use `wayfinder:<type>` labels for child tickets.
Types are `research`, `prototype`, `grilling`, and `task`.

To find the next ticket, list the map's open children.
Exclude tickets with an open blocker or an assignee.
Choose the first remaining ticket in map order.

Claim it with `gh issue edit <number> --add-assignee @me`.
Claiming is the session's first write.

To resolve a ticket, comment with the answer, then close it.
Append a context pointer with the gist and link to the map's Decisions-so-far.
