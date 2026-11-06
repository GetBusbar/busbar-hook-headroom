# Releasing busbar-hook-headroom

Development and release decisions belong to the private GitLab repository. GitHub is a public
source mirror. The former upstream polling, automatic dependency bump, tag, Docker bundle, and
plugin publication workflows are retired.

Update dependency pins and the lockfile together on `dev`, and run the existing checks through
internal CI. Promotion, signing, and publication require an explicit release request through the
internal release pipeline. Merging a branch or creating a tag does not trigger GitHub automation.
