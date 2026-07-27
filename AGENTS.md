## Overview
This repo contains various bits and bobs for (cosmic/wlroots-based) Wayland desktop shells.

## Notes
The `notes/` directory contains your persistent notes about the project state. Create/edit/rename/split/delete notes as needed (without being asked) to keep them correct and maximally useful to you. Keep notes concise, remove parts or whole notes that are unimportant or obvious. Keep `notes/README.md` up to date with an index of what is where.

## Issues
Issues live in `issues/`. Do not solve them unless asked or the fix falls out of current work. Create/update issues for nontrivial problems discovered during other work. Delete confirmed-solved issues (move still-useful context into notes first).

## Plans
Future plans live in `plans/`. Do not execute them unless asked, or write new plans unless asked. Like issues, delete them and integrate their contents into your notes when they are complete.

## Workflow
- This project is agent-built, you own the code.
- Refactor freely as needed. don't trust that existing code/comments/notes are necessarily correct, or existing design decisions are optimal.
- Only git commit when asked.
- Only pull/push when explicitly asked. Git push may hang without user approval.
- Commit to the current branch unless asked, don't make feature branches.
- Do not run code formatting tools unless explicitly asked.
- Keep prose, comments, errors, and commit messages short unless extra detail is genuinely useful.
- Avoid opening windows in the user's desktop, to test, interact with and screenshot GUI apps use the gui-testing skill from https://github.com/wmww/agent-skills.

## Coding guidelines
- Write safe, simple and correct Rust wherever possible.
- Use async Rust where it makes sense.
- Use `log`/`env_logger` for errors and other messages.
- The code should compile without warnings. Definitely harmless warnings (eg unused function/type) can be suppressed if the code is better that way.
- Use GTK4 and the gtk4-layer-shell crates as needed. This project is developed by the GTK Layer Shell developer, so if you run into issues with this library surface them.
- Write unit and integration tests for features and regression tests for bugs, use TDD when appropriate.
