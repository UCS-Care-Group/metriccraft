# Issue tracker: GitHub

Issues for this repo live as GitHub issues on `benandez/dashcraft`. Use the `gh` CLI for
all operations — the GitHub MCP server is not reliably connected.

## Conventions

- **Create**: `gh issue create --title "..." --body-file <file>` (heredoc to a file first;
  `--body` mangles multi-line text).
- **Read**: `gh issue view <n> --comments`
- **List**: `gh issue list --state open --json number,title,labels,assignees`
- **Comment**: `gh issue comment <n> --body-file <file>`
- **Label**: `gh issue edit <n> --add-label "..."` / `--remove-label "..."`
- **Close**: `gh issue close <n> --comment "..."`

## Wayfinding operations

The wayfinder map for this repo is **issue #1** (`wayfinder:map`).

- **Tickets** are GitHub **sub-issues** of the map, each with one `wayfinder:<type>`
  label: `research` (AFK), `grilling` (HITL), `prototype` (HITL), `task`.
- **Claim before working**: `gh issue edit <n> --add-assignee @me`. An open, unassigned
  ticket is unclaimed; assigning it is the claim.
- **Blocking** uses GitHub's native issue dependencies, so the frontier renders in
  GitHub's own UI.

Add a sub-issue (needs the database id, and `-F` for an integer field):

    gh api --method POST repos/benandez/dashcraft/issues/1/sub_issues \
      -F sub_issue_id=$(gh api repos/benandez/dashcraft/issues/<n> --jq .id)

Add a blocking edge (`<n>` is blocked by `<b>`):

    gh api --method POST repos/benandez/dashcraft/issues/<n>/dependencies/blocked_by \
      -F issue_id=$(gh api repos/benandez/dashcraft/issues/<b> --jq .id)

Find the frontier — open, unassigned, no open blockers:

    for n in $(gh issue list --state open --json number,labels                --jq '.[]|select([.labels[].name]|index("wayfinder:map")|not)|.number'); do
      blk=$(gh api repos/benandez/dashcraft/issues/$n/dependencies/blocked_by \
            --jq '[.[]|select(.state=="open")]|length')
      as=$(gh api repos/benandez/dashcraft/issues/$n --jq '.assignees|length')
      [ "$blk" = "0" ] && [ "$as" = "0" ] && echo "#$n"
    done

**Resolving a ticket**: post the answer as a comment, `gh issue close <n>`, then append a
one-line pointer to the map's *Decisions so far* section
(`gh issue edit 1 --body-file <edited>`).

Note: these are Bash-tool snippets. Anything handed to Martin to paste must be one-line
PowerShell — see the root `CLAUDE.md`.
