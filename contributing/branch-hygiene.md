# Branch and Worktree Hygiene

Guidance for agents asked to clean up local branches, worktrees, or the `.git` directory in a clone of this repo.

## One thing to know before you start

**The repo moves fast.** Branches that look dormant may be somebody else's in-flight work (cloud agents, other developers, other local worktrees). Merged branches get squash-merged, so `git branch --merged main` misses almost everything. Check PR state on GitHub before deleting anything with commits not in `main`.

## Before you run anything destructive

Read the user's memory for this repo at `~/.claude/projects/-Users-*-nteract-desktop/memory/` — there may be standing guidance like "only rebase/force-push branches I created this session" ([feedback_other_agent_branches.md](#)).

## Worktree cleanup

```bash
git worktree list                 # inventory
git worktree prune --dry-run -v   # what git already considers gone
git worktree prune -v             # remove the admin entries for those
```

### Worktree removal playbook

For each worktree that's still on disk:

1. `git -C <path> status --porcelain` — check for uncommitted work.
2. `git -C <path> log -1 --oneline` and `gh pr list --head <branch> --state all` — is the branch merged or still open?
3. If the worktree is **locked**: `git worktree remove` will refuse with "cannot remove a locked working tree, lock reason: claude agent agent-XXXX (pid NNN)". **Check if the PID is alive** (`ps -p NNN`) before breaking the lock — it may be another running agent. If the PID is dead, `git worktree remove -f -f <path>` force-unlocks and removes.
4. Never use `pkill`/`killall` to clear locks — per-worktree daemon isolation means those kill every agent on the box.
5. Removing a worktree does **not** delete the branch. `git branch -D <name>` separately, after confirming the branch is either merged or not worth keeping.

### What "safe to delete" looks like

- Worktrees under `/private/tmp/*`, `~/.codex/worktrees/*` — almost always ephemeral review/test workspaces. Check status, delete.
- Worktrees under `.claude/worktrees/agent-*` — created by superpowers agents. Lock reason names the dispatching agent + PID; dead PID = dead session.
- Branches whose PR is **MERGED** via `gh pr list --head <branch> --state all` — safe to delete (content is in main under a new squash SHA).
- Branches whose PR is **CLOSED** — ask before deleting, the user may intentionally revisit.
- Branches with no PR and commits not in main — **do not delete without asking.** These are often mid-flight work.

## Remote branch cleanup

This is the part people get wrong. `git branch -r` shows your **remote-tracking refs**, not what's on GitHub. Those drift apart as branches get deleted server-side.

### Reality check remote state first

```bash
git fetch --prune origin                # sync local remote-tracking refs with GitHub
git branch -r | wc -l                   # local's idea
gh api repos/nteract/desktop/branches --paginate --jq '.[].name' | wc -l   # GitHub's truth
```

If those two numbers disagree, trust `gh`. A 200-vs-9 split means local is stale, not that GitHub has 200 branches.

### Watch for multiple remotes

```bash
git remote -v
```

Clones sometimes accumulate two remotes pointing at the same GitHub repo (e.g. `origin` SSH and `https-origin` HTTPS). Deletions to one leave the other's remote-tracking refs untouched and looking alive. Either remove the duplicate (`git remote remove https-origin`) or `fetch --prune` both.

### Filtering remote branches by author

```bash
git for-each-ref --format='%(refname:short)|%(authoremail)' refs/remotes/origin \
  | awk -F'|' '$2=="<user@example.com>"' \
  | awk -F'|' '{print $1}' \
  | sed 's|^origin/||'
```

A branch-tip's author is the person who wrote the tip commit, which is not always who owns the branch (agents commit under bot identities like `Claude`, `Quill Agent`, `Cursor Agent`, `cursoragent@cursor.com`). Check open-PR authorship via `gh pr list --state open --json author,headRefName` before deleting by author.

### Open PRs are load-bearing

Deleting a remote branch that backs an open PR closes the PR and loses the review history on the head. **Always check:**

```bash
gh pr list --state open --limit 500 --json number,title,headRefName,author
```

Exclude every `headRefName` from the delete list.

### Deleting remote branches in bulk

`git push origin :branch-name` deletes. Batch with xargs (stay under ~50 refs per push so the remote doesn't reject the pack):

```bash
awk '{print ":" $0}' /tmp/remote-delete.txt > /tmp/refspecs.txt
split -l 50 /tmp/refspecs.txt /tmp/chunk-
for c in /tmp/chunk-*; do xargs git push origin < "$c"; done
```

"remote ref does not exist" means the branch was already gone on GitHub but your remote-tracking ref was stale. `fetch --prune` and rebuild the list.

## Local branch cleanup

Once you've decided what to delete:

```bash
git branch --list | grep -v '^\*' | awk '{print $1}' > /tmp/local-delete.txt
xargs git branch -D < /tmp/local-delete.txt
```

(macOS `xargs` doesn't support `-a` — use stdin redirection.)

Git refuses to delete the currently-checked-out branch, which protects `main`.

## Disk reclamation

After deleting refs, unreferenced objects still live in `.git/objects` until you GC.

```bash
git reflog expire --expire=now --all
git gc --prune=now --aggressive
du -sh .git .git/objects               # where is the weight?
```

Expect `.git/objects` to shrink to ~15M after aggressive GC on a fully-pruned clone. The bigger weight on disk is usually `target/` (cargo build cache) and `node_modules/`, which are gitignored — neither is touched by git GC.

### Long-lived refs that keep objects alive

`.git/packed-refs` and `.git/refs/` can carry namespaces beyond `refs/heads` and `refs/remotes` that anchor old commits:

- `refs/tags/*` — release tags, keep.
- `refs/codex/snapshots/*` — Codex CLI per-commit snapshots. Safe to drop if you don't need Codex's time-travel; they'll regenerate as needed.
- `refs/tmp/*` — rebase/merge temporaries. Usually safe.

List them:

```bash
awk '/^[0-9a-f]/{print $2}' .git/packed-refs | sed 's|/[^/]*$||' | sort | uniq -c | sort -rn
```

## What not to do

- **Don't force-push to `main`.** It's protected anyway.
- **Don't delete tags.** Releases depend on them.
- **Don't run `git clean -fdx`** — that nukes the `.venv`, `target/`, `node_modules`, the gitignored wasm + renderer-plugin outputs, and anything else not under version control. None of it is committed but losing it means a full rebuild (including `cargo xtask wasm`).
- **Don't delete branches authored by someone else without asking** — even bot branches (Claude/Cursor/Quill) may be scheduled work.

## Quick inventory cheatsheet

```bash
# Local
git worktree list                                    # worktrees
git branch --list | wc -l                            # local branches
git for-each-ref refs/remotes | wc -l                # remote-tracking refs (cached)

# Remote (truth)
gh api repos/nteract/desktop/branches --paginate --jq '.[].name' | wc -l
gh pr list --state open --limit 500 --json number,headRefName

# Disk
du -sh .git .git/objects
wc -l .git/packed-refs
```
