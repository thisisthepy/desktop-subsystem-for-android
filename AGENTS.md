# AGENTS.md

Guidance for working in this repository.

## Project

Desktop Subsystem for Android (DSA) runs Android in a dedicated lightweight VM and shows
each Android task as a real desktop window. Windows comes first; the protocol and the
guest are platform neutral so Linux and macOS hosts can follow.

- `dsa/`: all code. Nothing that builds lives outside it.
  - `dsa/protocol/`: the single definition of the guest-host control messages, and the
    generator for every language that speaks them.
  - `dsa/host/core/`: the platform-neutral host (Rust): sessions, the app registry, the
    lifecycle policy.
  - `dsa/host/platform/windows/`: the Windows host: the Android VM (HCS), hvsocket, the
    Win32 and DirectComposition compositor, TSF, shell integration. `unix/` holds what the
    Linux and macOS hosts share (the local API over Unix sockets); the rest of those hosts
    is planned.
  - `dsa/graphics/`: the Vulkan remoting path (C++), starting from gfxstream.
  - `dsa/guest/`: the Android side: the agent (a system app), the hwcomposer, the HAL
    clients, and the image configuration.
  - `dsa/kernel/`: the guest kernel configuration (binder built in) and its build.
- `docs/INTENT.md`: why the project exists, decisions (D1 to D13), rejected alternatives.
- `docs/RESEARCH.md`: the investigations behind decisions: what was checked, how, and what
  was found.
- `docs/SPEC.md`: requirements (`FR-*`, `NFR-*`, `PR-*`) with acceptance criteria.
- `PROJECT.md`: scope, milestones (M0 to M9), open questions.

The layout above is the agreed shape. Most of those folders do not exist yet; a folder is
created by the first change that puts real code in it.

## Spec Driven Development

1. **SPEC is the source of truth.** Before implementing a behaviour, find its SPEC ID. If
   none exists, stop and ask for one rather than writing the behaviour and a requirement for
   it in the same breath (2). Once it is agreed, it lands in its own commit, before the code.
2. **Never edit `docs/SPEC.md`, `docs/INTENT.md` or `PROJECT.md` without being asked to.**
   Not a requirement, not an acceptance criterion, not a decision, not a status. Propose the
   change, say what it costs and what it buys, and wait. This holds even when the change
   looks like a correction, and it holds hardest when the code cannot do what the document
   says: the requirement is not the thing that gives way. A requirement that moves whenever
   the code cannot meet it records nothing.

   Fixing an ID that points at a renamed test, or a link that has rotted, is not this.
   Changing what the project promises is.
3. If code and SPEC disagree, fix the code. If the SPEC is wrong, say so and ask; do not
   edit it and carry on.
4. Decision changes go to `docs/INTENT.md` first, then SPEC, then code, and the decision
   itself is the owner's to make (2).
5. When a requirement's acceptance criteria are verified, update its status (`Draft` to
   `Agreed` to `Done`) in the same change that proves it. A status is a statement about
   evidence, so this one follows the evidence rather than needing to be asked for; every
   other edit to those documents does. `Draft` to `Agreed` is the owner's agreement, not
   evidence, so it is never made on an agent's own judgement.
6. Resolved open questions are removed from `PROJECT.md` and recorded as decisions in
   INTENT and SPEC, once the question has actually been answered by the person who asked it.

## Branches

`develop` is where work happens. `release` and `main` are produced from it by
`.github/scripts/release/sync-release.sh`, which strips `PROJECT.md`, `AGENTS.md`,
`CLAUDE.md` if present, and everything directly under `docs/`. A file on `main` must
therefore never depend on those documents; a comment may name them only as being on
`develop`.

1. **Publishing is one way: develop to release to main.** A push to `develop` runs "Release
   sync", which rebuilds `release` and opens a pull request from `release` into `main`. A
   person merges it. Never merge `release` or `main` back into `develop`: their history
   contains the deletion of the planning documents, and merging it replays that deletion as
   if someone intended it.
2. **Start every branch and worktree from `develop`.** A worktree created from the default
   branch starts on `main`, which has neither the planning documents nor the recent work.
   Check with `git log --oneline -1` before touching anything. Merging `develop` in
   afterwards is not equivalent: the merge carries main's deletions with it.
3. `scripts/tests/planning-docs.test.sh` fails if those documents go missing, so the loss is
   caught rather than discovered weeks later.
4. **Every worktree builds into its own build directories.** Two checkouts sharing one
   build directory run each other's binaries. Never set `CARGO_TARGET_DIR` or a shared
   output directory anywhere that outlives a single command.
5. **Prune worktrees you are not using.** Android images, the kernel and gfxstream are large
   builds, and a forgotten worktree holds gigabytes.
6. **One worktree per agent, and never two workers in the same checkout.** Switching
   branches changes every file under that checkout, so a `git checkout` while an agent is
   working pulls the files out from under it.
7. **Work branches are named `feat/<topic>`.**
8. **Merge with `gh pr merge --delete-branch`**, then remove the local branch and its
   worktree.
9. **Only `main`, `develop` and `release` stay on the remote.** A branch merged into
   `develop` is deleted; its commits are in `develop`, so nothing is kept for it.

## CI naming

Workflow files and names, in `.github/workflows/`:

- `test.yml`, "Test": the basic checks, on every pull request.
- `release-sync.yml`, "Release sync": develop to main sync; its script is
  `.github/scripts/release/sync-release.sh`.
- `test-<target>.yml`, "<Target> test": a special check for one target, such as
  `test-kernel.yml` for the guest kernel.

Job names are English sentence case and say briefly what the job does. Matrix jobs read
"<what> (<os>, <version>)". The same role keeps the same name across repos.

## Where files go

1. **Everything this project makes stays inside this repository.** Worktrees, experiments,
   probes, scratch files, build outputs, and anything downloaded to build or test with. Not
   `/tmp`, not a directory beside this checkout, not the home directory. The owner has to be
   able to find all of it in one place and remove it by removing one place.
2. Where inside:
   - worktrees: `.claude/worktrees/<name>/` (ignored by git);
   - throwaway work, probes and downloads: `.scratch/<name>/` (ignored by git);
   - what an experiment found: a section in `docs/RESEARCH.md` (on `develop` only), with
     the versions checked, the method, the results and the decision. The probe itself stays
     in `.scratch/` and is not committed;
   - build outputs: `target/`, `build/` and `out/`, where the tools already put them.
3. Upstream projects (AOSP, gfxstream, the WSL kernel, Waydroid) are not vendored. A
   script fetches the pinned revision into `.scratch/` and applies the patches this
   repository keeps. Waydroid's tools are GPL-3.0 and this repository is Apache-2.0, so no
   Waydroid code is copied in.
4. Caches a toolchain keeps for itself (`~/.cargo`, `~/.rustup`, `~/.gradle`, the Android
   build's own caches) belong to the tool and stay where it puts them. Downloading or
   installing a tool this project does not already use is not a cache: ask first, and put
   it inside `.scratch/`.
5. If a task seems to need a path outside the repository, ask before writing there. Being
   asked and told yes is the only exception, and it covers that path for that task.

## Repository root

1. **Nothing new goes in the root without the owner's approval.** Not a folder, not a file.
   Propose it first: what it is, why it is needed, and why no existing directory can hold
   it. Then wait for the answer.
2. The approved root entries are:
   - folders: `.github/`, `dsa/`, `docs/`, `scripts/`;
   - files: `.gitignore`, `CHANGELOG.md`, `LICENSE`, `README.md`, `AGENTS.md`, `PROJECT.md`.

   A workspace manifest (`Cargo.toml`, `Cargo.lock`) is not approved yet. Rust crates keep
   their manifests inside `dsa/` until the owner agrees to a root workspace.

   Ignored local directories (`.claude/`, `.scratch/`, `target/`, `build/`, `out/`) are not
   part of the tree and are covered by "Where files go" above.
3. A crate, library or tool that needs a home goes inside the folder it belongs to
   (`dsa/host/core/`, `dsa/protocol/codegen/`), not beside it.
4. Scripts that only CI runs live in `.github/scripts/`. `scripts/tests/` is the one test
   folder outside `dsa/`.

## Background agents

These bind the agent and whoever dispatches it equally.

1. **A background agent never runs the heavy builds.** Not the Android system image, not the
   guest kernel, not gfxstream. Those builds take tens of minutes to hours, use most of the
   machine's cores and memory, and several running at once take the machine down. Do not
   write such a build command into a task prompt. When a build or a test run is needed, one
   temporary builder agent does it, one at a time, in its own worktree, and edits no code
   beyond the minimal fixes a build needs, which it reports with the commands it ran and
   their output. When no builder is needed, push and let CI build.
2. **A background agent never decides that part of its task is out of scope.** If the task
   says implement it, it gets implemented. No narrowing, no deferring, no "future work", no
   TODO standing in for the feature, and above all no writing that judgement into SPEC or
   INTENT as though it were settled.

   Disagreeing is allowed and wanted. Deliver everything else in full, then say in the final
   report what was not done and why. The failure is the silence, not the disagreement.

   **The final report is not a licence.** Saying what was skipped does not discharge the
   obligation to do it. That clause is for work that turned out to be impossible or blocked,
   not for work judged not worth doing. "Nobody asked for it" and "the task did not name it"
   are not blockers.

   **Nothing that the thing cannot work without is out of scope.** The deliverable is the
   working whole, not the part the task happened to name. A compositor that compiles but that
   no host process starts is not delivered. The test is whether someone can use what you
   built without writing the missing piece themselves.
3. **Whoever reads the report is bound by the same rule.** An "out of scope" in a report is
   not an answer to be relayed onward; it is a thing to send back.

## Writing

The house style is `docs/style/writing.md` in `thisisthepy/pythonx-compose`, on its
`develop` branch
([source](https://github.com/thisisthepy/pythonx-compose/blob/develop/docs/style/writing.md)).
It is the standard for every document here: README files, the planning documents, doc
comments. In short:

- Present tense for what is true now. What is not done carries its status and the issue that
  tracks it, so a plan never reads as shipped.
- Short, plain sentences, one idea each. A rule or a limitation comes with its reason.
- English and Korean say the same things in the same order, written naturally in each
  language rather than translated word for word.
- Korean user docs (`docs/locales/`) use 합니다체; internal docs use 한다체.
- Code, commands, file names and identifiers keep their original spelling, in code format.

Apply it to the lines you write or change. A whole-document rewrite is its own task, not a
side effect of an unrelated edit. The rules below come on top of it.

1. **Never use em dashes.** Not in docs, code comments, commit messages, pull request text or
   UI copy. Use a comma, a colon, parentheses, or start a new sentence. Hyphens in compound
   words and en dashes in numeric ranges are fine. `scripts/tests/no-em-dash.test.sh` checks
   it.
2. Language: `README.md` is English, with its translation in `docs/locales/README_ko.md`.
   The internal planning documents (`PROJECT.md`, `docs/INTENT.md`, `docs/SPEC.md`,
   `docs/RESEARCH.md`) are
   Korean. Code, code comments, scripts and this file are English.
3. **README files never link the planning documents.** They are stripped from `main`, so the
   link would point at nothing for most readers.
4. **Never cite SPEC or INTENT from code.** No `(SPEC PR-2)`, no `INTENT D7 needs this`, in
   comments, error messages, log lines or assertion text. Those documents are stripped from
   the published branch, so a citation points at nothing for most readers, and it goes stale
   the moment a requirement is renumbered. Say the actual reason instead:
   `"schema hash mismatch (PR-2)"` becomes
   `"the agent was built from a different protocol definition (hash 1a2b, expected 3c4d)"`.
   The test is whether someone with no access to the planning documents can act on what you
   wrote.

   Two exceptions, both deliberate: **commit messages and pull request text** may reference
   requirement IDs, because they are addressed to people working in this repository, and
   **test names** (`pr2_mismatched_schema_hash_is_rejected`) keep theirs, because the
   traceability from a failing test to its requirement is the point of the TDD rules below.
   Anything a test *prints* follows the rule above.
5. **Trademarks.** The product is "Desktop Subsystem for Android" or DSA. Never name it, a
   component, a binary or an icon with "Windows" or another Microsoft mark, and never put
   "Android" first in a name. The first mention of Android in a user-facing document is
   "for Android™", and the document carries "Android is a trademark of Google LLC."
6. **Python-side tooling in examples is uv, ppp (pypackpack) and tcl (toolchain-lite) only.**
   Never `pip install`, `python -m pip` or a bare `python3 script.py`. The reason is one
   toolchain story across the thisisthepy projects.

## Test Driven Development

SDD says what to build; TDD is how it gets built. Tests come from SPEC acceptance criteria,
so a requirement without a test is not done.

1. **Red, green, refactor.** Write the failing test first, make it pass with the simplest
   change, then clean up with the test still green.
2. **Name tests after the requirement**: `pr2_mismatched_schema_hash_is_rejected`,
   `fr10_closing_last_window_enters_doze`. A reader should be able to go from a failing test
   to the SPEC line it defends.
3. **No production change without a test that would have caught its absence.** A bug fix
   starts with a test that reproduces the bug.
4. **Commit order**: the test and the code that makes it pass go in the same commit (the tree
   must build green at every commit). Say in the commit body which SPEC criterion it covers.
5. **Test through the public surface**: the protocol, the host's API, the agent's control
   channel. Do not assert on internals that the SPEC does not describe.
6. **Fakes, not mocks of our own protocol.** Use the in-memory transport and assert against
   the checked-in protocol vectors, so the Rust, Kotlin and C++ sides stay in lockstep.
7. **Performance is a test too.** NFR budgets are measurements with recorded numbers and the
   environment they were taken in.

Not everything can be automated. IME behaviour, resizing and the clean install are manual
checklists (SPEC §6). When a requirement can only be checked by hand, say so in the SPEC item
instead of leaving it untested silently.

## Hard constraints (INTENT §2)

Never introduce anything that violates these. If a task seems to require it, stop and ask.

- **Open source only.** No Microsoft WSA binary, modified or not, and nothing whose licence
  cannot ship beside Apache-2.0.
- **One host window per Android task.** No phone-frame emulator window showing the whole
  Android screen.
- **No WSLg in the display path.** No Weston, no RDP, no XWayland between a task and its
  window.
- **Idle memory must be reclaimable by the host.** Nothing that pins guest memory while idle.
- **Trademark rules** as in Writing (5).
- **No Google Play services or store shipped by the project.** Leave them as a user's choice.

## Architecture rules

- **A dedicated VM, not the user's WSL (D1).** Android runs in its own lightweight VM, created
  through HCS with WSL's memory return settings. Never change the user's `.wslconfig`, WSL
  kernel or WSL distributions. A WSL container is only a later, second backend.
- **The project ships the kernel (D2).** The guest kernel has binder built in and ships with
  DSA. Never require a user to build or replace a kernel. Do not look up unexported kernel
  symbols at run time. Do not depend on ashmem; use memfd.
- **Display path (D3).** Freeform tasks, a custom hwcomposer that hands each task's layers to
  the host, and a host compositor that owns one native window per task. Control messages go
  over the control channel, never over adb.
- **Graphics (D4).** Every app's OpenGL ES driver is ANGLE. Vulkan runs on the host through
  remoting. The dxg path is research, recorded in `docs/RESEARCH.md`, until the owner
  decides otherwise.
- **The boundary is the HAL (D5).** The kernel, ART and the framework stay in the guest.
  Devices move to the host as HAL services over RPC Binder. Never forward system calls.
- **The host is the shell (D6).** No launcher, no SystemUI, no keyboard app in the guest
  image. Text input goes through the host IME and the agent's `InputConnection`; never
  round-trip composition through a guest keyboard.
- **Lifecycle (D7).** Doze is the idle state. Freeze is opt-in. Do not add a code path that
  freezes the guest by default.
- **Platform neutral core (D9).** The protocol, the guest and `dsa/host/core/` never name a
  host OS API. Everything platform specific is under `dsa/host/platform/<os>/`.
- **One protocol definition (PR-2).** Codecs are generated, never hand written. Every
  connection checks the protocol version and schema hash.
- **Crash isolation (NFR-5).** A bad message or a dead guest never takes the host process
  down. Report it and keep running.

## Commits

- **One logical change per commit.** Split work by feature or requirement. Do not mix SPEC
  edits, refactors, and features in one commit.
- Subject format: `<Type>: <imperative summary>`
  - `Feat`: new functionality
  - `Fix`: bug fix
  - `Refactor`: behaviour-preserving code change
  - `Docs`: README, PROJECT, INTENT, SPEC, AGENTS.md
  - `Test`: tests only
  - `Chore`: build, tooling, dependencies, config
- Reference SPEC IDs in the subject or body when relevant, e.g.
  `Feat: Reject a connection whose schema hash differs (PR-2)`.
- **Do not add `Co-Authored-By` trailers or any AI attribution** to commits or pull request
  descriptions.
- Commit only when asked, or when a task explicitly includes committing. Never force-push or
  rewrite published history without permission.
- Do not commit `.DS_Store`, build outputs (`target/`, `build/`, `out/`), downloaded
  upstream trees, or system images.

## Verification

- A milestone is complete only when its SPEC acceptance criteria pass. Report failures with
  actual output; do not mark items `Done` on assumption.
- Manual checks (SPEC §6) run on an installed build on real Windows hardware, and the result
  is recorded with the build and the machine.
- Performance claims (frame time, CPU, memory, start time) require measured numbers, recorded
  alongside the relevant SPEC item with the environment they were taken in.
