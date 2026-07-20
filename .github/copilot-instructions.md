# Copilot / AI agent instructions for `FalkorDB/benchmark`

Guidance for GitHub Copilot and other AI agents working in this repository. It encodes the
team's engineering conventions so changes land clean on the first try. Human contributors
should follow it too.

`benchmark` is FalkorDB's benchmarking tool. It has two parts:

- a **Rust CLI** (`src/`, the `benchmark` binary) that loads data into and drives workloads
  against **FalkorDB**, **Neo4j** and **Memgraph**, exposing Prometheus metrics; and
- a **Next.js dashboard** in **`ui/`** that renders the results, with Playwright smoke tests.

The default branch is **`master`**. The FalkorDB Rust client is pulled in from
`vendor/falkordb-rs` via a Cargo `[patch]`, so keep it out of lint/format passes.

## Golden rule: drive everything through `just`

For **any** check that CI performs — Rust `build`, `clippy`, `test`, `coverage`, the Markdown
`doc-check`, and the Playwright UI smoke test — run the **exact same `just` recipe CI uses**, never
a raw `cargo …` / `npm …` command (`just fmt-check`, `just ui-lint` and `just ui-build` are
recommended local recipes but not CI gates). If a check needs changing, update the `just` recipe
**and** the CI workflow together so they stay identical. Run `just --list` to see every recipe.

Key recipes:

| Recipe | Purpose |
| --- | --- |
| `just check` | Fast pre-commit loop for Rust: `fmt clippy build`. |
| `just ci` | Every Rust CI gate, in the same order CI runs them: `build clippy test`. |
| `just clippy` | Strict clippy, warnings denied, scoped to the `benchmark` package (the `clippy` CI gate). |
| `just build` | Build all targets/features (the `build` CI gate). |
| `just test` | Unit + integration tests (the `test` CI gate). Also compiles the Markdown Rust examples as doctests (see `src/doc_examples.rs`). |
| `just test-one <filter>` | Run a single test by name filter. |
| `just doc-check` | All Markdown doc checks (the `Docs validation` workflow): `doc-links` + `doc-shell`. |
| `just doc-links` | Offline broken-link + anchor check (lychee) over every tracked `*.md` except `vendor/`. |
| `just doc-shell` | Syntax-check (`bash -n`, no execution) the `bash`/`sh` examples in the Markdown docs. |
| `just coverage` | Codecov JSON coverage via cargo-llvm-cov, including the `#[ignore]`d integration tests (the `coverage` CI job). Needs a reachable FalkorDB — set `FALKORDB_HOST`/`FALKORDB_PORT` or use `just coverage-local`. |
| `just coverage-local` | Spin up a Docker FalkorDB, run `just coverage`, then tear it down. |
| `just coverage-html` | Open a browsable HTML coverage report locally (also needs a FalkorDB). |
| `just synthetic-bench` | Run the synthetic per-operation latency probe (needs a live FalkorDB). |
| `just synthetic-ops` | List the synthetic operations. |
| `just synthetic-it` | Run the synthetic integration test against a live FalkorDB. |
| `just fmt` / `just fmt-check` | Format Rust in place / check formatting. |
| `just run -- <args>` | Run the benchmark binary (e.g. `just run -- --help`). |
| `just ui-install` | `npm ci` in `ui/`. |
| `just ui-lint` / `just ui-build` | Lint / production-build the dashboard. |
| `just ui-smoke` | The Playwright CI smoke test (the `Playwright Tests` gate). |
| `just bench-small` / `bench-medium` / `bench-large` | Run the dataset benchmark pipelines in `scripts/`. |

Each **CI job only adds environment setup (protobuf / Node / coverage tooling) then installs
`just` and runs one recipe per check step** — so whatever CI checks, you can reproduce locally with
the identical recipe. If you add or change a CI check, add or change the recipe and wire the
workflow to call it; never inline a bare command in a workflow.

## Working on the UI (`ui/`)

`just ui-*` recipes wrap `npm` in `ui/`. **Never run `just ui-build` (or `next build`/`next start`)
while `just ui-dev` (`next dev`) is live** — both write `ui/.next` and corrupt it (ENOENT
manifest/rename errors). If that happens, `rm -rf ui/.next` and restart. `just ui-smoke` starts its
own dev server, runs the smoke spec and tears the server down; run `just ui-install` first.

## Definition of done for a change

1. **Design first** for non-trivial work, and **rubber-duck review** the design before coding.
2. **Implement** the change with code **+ tests + docs**. Cover new code with tests (see
   **Coverage** below). On every change, **check and align all documentation** (the README, recipe
   doc-comments, this file) so it never drifts from the code — see **Keep documentation in sync**.
3. **Run all validations locally before committing** — never commit without a green local run.
   Run `just ci` (build, clippy, test) **and** `just coverage`, plus `just ui-lint` /
   `just ui-build` / `just ui-smoke` when the UI changed.
4. Open a PR on a feature branch (prefix with your username, e.g. `barakb/…`) targeting `master`.
5. **Resolve every AI review thread** (Copilot **and** CodeRabbit) — reply *and* mark it resolved —
   before merge. Copilot's reviewer does not reliably re-review new commits; re-request it (POST
   `pulls/{n}/requested_reviewers`) after pushing. CodeRabbit auto-reviews each push.
6. **Never merge to `master` yourself — wait for explicit human approval.** Do **not** run any
   `gh pr merge …` variant to self-merge, even when every check is green and all AI threads are
   resolved. Open the PR, get it green, and **stop** until the maintainer approves the merge.

## Keep documentation in sync

On **every** change, check and align **all** documentation so it never drifts from the code —
treat "the docs match the code" as part of the definition of done, not a follow-up:

- Update **`readme.md`** whenever behavior, commands, CLI flags, `just` recipes, or setup steps
  change — keep the **Development** (just recipes for tests/coverage) section and any examples
  current.
- Update the **recipe doc-comments** in the `Justfile` and the recipe tables in this file
  (`.github/copilot-instructions.md`) whenever you add, rename, or change a recipe or a workflow.
- Keep the other docs (`ui/README.md`, `QUERY_EXPLANATIONS_AND_SAMPLES.md`, …) accurate when the
  code they describe changes.

## Coverage

Write **as much test coverage as possible** — cover new code with unit tests and keep patch
coverage high. **Patch coverage must be ≥ 90%** (enforced by Codecov via `codecov.yml`): any diff
that drops below fails the `codecov/patch` check, so cover new lines before you push. Measure it
with the exact CI command, **`just coverage`** (cargo-llvm-cov → `codecov.json`), not an ad-hoc
line count; **`just coverage-html`** opens a browsable report. `just coverage` runs the
`#[ignore]`d integration tests too (`--include-ignored`), so it needs a reachable FalkorDB — use
**`just coverage-local`** to spin one up in Docker automatically (CI provides a FalkorDB service).
Coverage is uploaded to Codecov by the `coverage` workflow (see `codecov.yml` for thresholds).
Prefer testing real logic (parsing, query building, scheduling, aggregation) with unit tests, and
cover server-backed paths with integration tests that run under coverage against that FalkorDB.

## Flaky tests are a hard no

Fix a flaky test **immediately, as top priority**, regardless of the current task or whether the
flake is pre-existing. Find the root cause (retry only the genuinely transient error and fail fast
on everything else) rather than papering over it.

## Feature flags over deletion

When temporarily disabling a feature (e.g. because of an upstream bug), gate it behind a flag so it
is non-accessible from the API and the UI rather than deleting the code — so it can be re-enabled
easily once fixed.

## Commits & PRs

- Use clear, imperative commit subjects. Prefer **Conventional Commits** (`feat`, `fix`, `ci`,
  `docs`, `chore`, `refactor`, `perf`, `test`, `build`) where it helps.
- Include a `Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>` trailer on
  agent-authored commits.
- Keep PRs focused; don't reformat or refactor unrelated code in the same change.

## Code style

Keep code **tidy, simple, and efficient**. Comment only what genuinely needs clarification — not
the obvious. Match the surrounding style; prefer the smallest change that fully solves the problem.
