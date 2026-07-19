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

For **any** check that CI performs — Rust `build`, `clippy`, `test`, and the Playwright UI smoke
test — run the **exact same `just` recipe CI uses**, never a raw `cargo …` / `npm …` command
(`just fmt-check`, `just ui-lint` and `just ui-build` are recommended local recipes but not CI
gates). If a check needs changing, update the `just` recipe **and** the CI workflow together so
they stay identical. Run `just --list` to see every recipe.

Key recipes:

| Recipe | Purpose |
| --- | --- |
| `just check` | Fast pre-commit loop for Rust: `fmt clippy build`. |
| `just ci` | Every Rust CI gate, in the same order CI runs them: `build clippy test`. |
| `just clippy` | Strict clippy, warnings denied, scoped to the `benchmark` package (the `clippy` CI gate). |
| `just build` | Build all targets/features (the `build` CI gate). |
| `just test` | Unit + integration tests (the `test` CI gate). |
| `just fmt` / `just fmt-check` | Format Rust in place / check formatting. |
| `just run -- <args>` | Run the benchmark binary (e.g. `just run -- --help`). |
| `just ui-install` | `npm ci` in `ui/`. |
| `just ui-lint` / `just ui-build` | Lint / production-build the dashboard. |
| `just ui-smoke` | The Playwright CI smoke test (the `Playwright Tests` gate). |
| `just bench-small` / `bench-medium` / `bench-large` | Run the dataset benchmark pipelines in `scripts/`. |

Each **CI job only adds environment setup (protobuf / Node) then installs `just` and runs one
recipe per check step** — so whatever CI checks, you can reproduce locally with the identical
recipe. If you add or change a CI check, add or change the recipe and wire the workflow to call
it; never inline a bare command in a workflow.

## Working on the UI (`ui/`)

`just ui-*` recipes wrap `npm` in `ui/`. **Never run `just ui-build` (or `next build`/`next start`)
while `just ui-dev` (`next dev`) is live** — both write `ui/.next` and corrupt it (ENOENT
manifest/rename errors). If that happens, `rm -rf ui/.next` and restart. `just ui-smoke` starts its
own dev server, runs the smoke spec and tears the server down; run `just ui-install` first.

## Definition of done for a change

1. **Design first** for non-trivial work, and **rubber-duck review** the design before coding.
2. **Implement** the change with code **+ tests + docs**. On every change, **check and align the
   documentation** (README, recipe docs, this file) so it never drifts from the code.
3. **Validate locally via `just`** — all relevant gates green (`just ci`, plus `just ui-lint` /
   `just ui-build` / `just ui-smoke` when the UI changed).
4. Open a PR on a feature branch (prefix with your username, e.g. `barakb/…`) targeting `master`.
5. **Resolve every AI review thread** (Copilot **and** CodeRabbit) — reply *and* mark it resolved —
   before merge. Copilot's reviewer does not reliably re-review new commits; re-request it (POST
   `pulls/{n}/requested_reviewers`) after pushing. CodeRabbit auto-reviews each push.
6. **Never merge to `master` yourself — wait for explicit human approval.** Do **not** run any
   `gh pr merge …` variant to self-merge, even when every check is green and all AI threads are
   resolved. Open the PR, get it green, and **stop** until the maintainer approves the merge.

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
