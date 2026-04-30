# Protocol — Spec

A workspace-aware project shell. The app manages **workspaces** (named bundles of git repos) and **instances** (cloned materializations of a workspace tied to a logical change). Layout mimics Zed's: left project panel, main content pane, status bar.

No AI for v1.

---

## Concepts

### Workspace

A named bundle of git repositories. Defined by a TOML file under `~/.protocol/workspaces/<name>.toml`:

```toml
name = "acme"

[[repo]]
name = "api"
url  = "git@github.com:acme/api.git"
default_branch = "main"

[[repo]]
name = "web"
url  = "git@github.com:acme/web.git"
default_branch = "main"
```

Hand-edited for v1. UI for editing comes later.

### Instance

A directory containing one fresh `git clone` of every repo in the workspace, each checked out to that repo's `default_branch` from the workspace config.

```
~/.protocol/instances/<workspace>/<instance>/
    api/   (cloned repo)
    web/   (cloned repo)
```

No metadata file. Instance state is derivable from disk. The instance's branches live in each repo's `.git`.

**Lifecycle:**
- Create: pick a name → app `mkdir`s the directory → clones every repo on its `default_branch` → refresh UI when done.
- Delete: right-click in instance list → confirm → `rm -rf` → refresh.
- Failures during clone: leave partial directory, surface error, let user retry or delete.

---

## Layout (Zed-style)

```
┌────────────────────────────────────────────────────────────────┐
│ ⌘ acme / payments-v2 ▾                              [traffic] │   title bar
├──────────────────┬─────────────────────────────────────────────┤
│ ▾ PROJECT        │                                             │
│   ▸ api          │                                             │
│   ▸ web          │     main pane (file content, read-only)     │
│                  │                                             │
│ ▾ WORKSPACES     │                                             │
│   ◦ acme  ●      │                                             │
│   ◦ infra        │                                             │
│                  │                                             │
│ ▾ INSTANCES      │                                             │
│   ◦ payments-v2 ●│                                             │
│   ◦ login-flow   │                                             │
│   + new instance │                                             │
├──────────────────┴─────────────────────────────────────────────┤
│ branch: main · clean                                           │   status bar
└────────────────────────────────────────────────────────────────┘
```

Left panel: fixed width (~280px), three collapsible sections — file tree of active instance, workspace picker, instance picker. Right-click on an instance row → "Delete instance".

Main pane: file content (read-only) or placeholder.

Status bar: branch + clean/dirty for the file under the cursor.

---

## v1 chunks (ordered)

1. **App skeleton.** Left panel + main pane + status bar with fixed dummy content. No data, no logic. Verify visually.
2. **Config loader.** Read `~/.protocol/workspaces/*.toml`. Surface parse errors. No UI yet.
3. **Workspace + instance pickers.** Render the loaded workspaces and the filesystem-scanned instances in the left panel. Click to select. Selection state lives in the root entity.
4. **File tree.** `uniform_list` over the active instance directory. Lazy-expand folders. Each repo is a top-level node.
5. **File viewer.** Click a file → read contents → render as monospace text in the main pane. No syntax highlighting, no editing.
6. **Create instance.** "+ new instance" → name modal → background clone task → progress in status bar → refresh on done.
7. **Delete instance.** Right-click → confirm → `rm -rf` → refresh.
8. **Title bar.** Custom titlebar showing `<workspace> / <instance>`. Click to popover-switch (Zed style).
9. **Status bar.** Show current branch + clean/dirty of the repo containing the selected file.

---

## Out of scope for v1

- Editing files (read-only viewer only).
- Syntax highlighting, diff view, multi-file tabs.
- Git operations beyond clone (no commit / branch / push UI).
- AI integration of any kind.
- Workspace config editor (hand-edit TOML).
- Multi-window.
- Settings UI.
- Cross-platform polish (macOS-first; Linux/Windows after the model is right).

---

## Decisions locked

- Storage root: `~/.protocol/` (visible, hand-editable; not `~/Library/Application Support/`).
- Clone via `git2` (libgit2). Real progress callbacks, no CLI parsing.
- Partial-clone failures leave the directory; user retries or deletes.
- Persist last-selected workspace + instance in `~/.protocol/state.toml`.
- File tree expands lazily.
- Single root per repo (no submodule special-casing).
