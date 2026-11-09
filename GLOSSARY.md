# Bitbucket repository and pull request workflow

This context covers repository discovery, pull requests, and review decisions in Bitbucket Cloud.

## Language

**Workspace**:
A Bitbucket grouping of repositories and their access permissions.
_Avoid_: Repository, Atlassian organization

**Repository**:
A Git repository hosted in a Bitbucket workspace.
_Avoid_: Project when referring to a repository

**Repository name**:
The human-readable display name of a repository, distinct from its repository slug.
_Avoid_: Repository slug

**Repository slug**:
The workspace-local repository identifier used in Bitbucket repository URLs.
_Avoid_: Repository name

**Accessible repository**:
A repository whose metadata the authenticated user's current credentials permit them to read.
_Avoid_: Public repository as a synonym

**Pull request**:
A proposal to merge changes from a source branch into a destination branch for review.
_Avoid_: Merge request

**Source branch**:
The branch containing the changes proposed by a pull request.
_Avoid_: Destination branch

**Destination branch**:
The branch into which a pull request proposes to merge its changes.
_Avoid_: Source branch

**Approval**:
An affirmative review decision that a Bitbucket user records on a pull request.
_Avoid_: Merge permission, merge readiness

**Approver**:
A Bitbucket user with a current approval recorded on a pull request.
_Avoid_: Reviewer when referring specifically to a user who has approved

**Reviewer**:
A Bitbucket user designated to review a pull request. Being a reviewer does not mean the user has approved it.
_Avoid_: Approver when referring only to a review assignment

**Approval withdrawal**:
The removal of a user's current approval from a pull request.
_Avoid_: Rejection, request for changes

**Authenticated user**:
The Bitbucket user whose credentials authorize an action and whose identity the action uses.
_Avoid_: AI identity, anonymous bot

**Inline comment**:
A pull request comment attached to a location in a file within the proposed changes.
_Avoid_: General comment when referring to a file location

**General comment**:
A comment on the pull request as a whole, without an attachment to a file location.
_Avoid_: Inline comment

**Reply**:
A pull request comment that responds to another comment.
_Avoid_: New thread when referring to a response in an existing thread

**Merge readiness**:
Whether a pull request meets the conditions required for a merge.
_Avoid_: Approval when referring to all merge conditions
