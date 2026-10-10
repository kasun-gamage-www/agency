# Agency Desktop

A native desktop app (Rust + egui) for managing several git repositories as multi-repo **projects**.

## Features

- **Workspace**: add repositories with a folder picker (multi-select), or scan a folder to add every repo found up to two levels deep.
- **Projects**: create, rename and delete projects, and tick which repositories belong to each. A repository can be in more than one project.
- **Project overview**: shows each repo's branch, ahead/behind count against its upstream, and number of changed files. Fetch, pull or push all repos at once, or one at a time.
- **History**: commit graph across all branches or the current one, with filtering by message, author, hash or ref. Click a commit to see the full `git show` with a coloured diff. Right-click to copy its hash or check it out.
- **Changes**: staged and unstaged file lists with per-file diffs. You can stage or unstage one file or all of them, discard changes (asks first), and commit or amend.
- **Branches**: check out local or remote branches (remote ones get a tracking branch), create a branch, merge into the current branch, and delete a branch. Stash, then apply, pop or drop stashes.
- Fetch, pull and push run in background threads, so the UI never freezes. Results go to the **Activity** panel, and errors appear in a dialog.

The app runs the `git` CLI you already have installed, so your existing credentials, SSH keys and config are used. `git` must be on `PATH`.

## Run

```sh
cargo run --release
```

## Test

```sh
cargo test
```

The test creates a throwaway repository in the temp directory and runs every git operation the app uses.

## Data

Projects and the repository list are saved to `%APPDATA%\agency-desktop\workspace.json` on Windows (`~/.config/agency-desktop/` on Linux, `~/Library/Application Support/agency-desktop/` on macOS).

## Layout

| File | Purpose |
| --- | --- |
| `src/main.rs` | Window setup |
| `src/app.rs` | UI. Widgets emit `Action`s, which are applied after each frame |
| `src/git.rs` | Wrappers and output parsers for the git CLI, plus tests |
| `src/store.rs` | Saving and loading projects and the repository list |
