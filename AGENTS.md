# Instructions for agents

rootsqz is a Rust executable that compresses demo intros into one self-extracting HTML file, plus a pnpm workspace with its npm packages. The [Development section of the README](README.md#development) describes the layout, the build and the release.

## Keep the changelog up to date
Every user-facing change gets an entry in [CHANGELOG.md](CHANGELOG.md), in the same commit as the change:
- Add it under `## [Unreleased]` at the top, in the subsection `Added`, `Changed`, `Fixed` or `Removed`.
- Write for users of the CLI, the Node API and the plugin, not about the implementation. Mark breaking changes with `**Breaking:**` and say what to change.
- Changes that users don't notice, such as refactoring, tests or CI, need no entry.

On a release, `## [Unreleased]` becomes `## [<version>] - <date>`, and a link reference for the version is added at the end of the file. The release workflow fails without a section for the tagged version, and uses the section as the notes of the GitHub release.

## Build and test
```sh
cargo build --release
cargo test --release
ROOTSQZ_BINARY_PATH=$PWD/target/release/rootsqz pnpm run build   # packages and the vite example
```
CI (`.github/workflows/build.yml`) also runs `node scripts/set-version.mjs --check`.

## Conventions
- **Version:** the crate and all npm packages share one version. Change it only with `node scripts/set-version.mjs <version>`.
- **Output size matters most:** a new feature of the generated output must not add bytes to outputs that don't use it.
- **Documentation:**
  - Public interfaces of the npm packages have JSDoc comments.
  - The top-level README and `packages/rollup-plugin-rootsqz/README.md` have the same plugin section, so update both.
  - Changes to the command line options also go in `packages/rootsqz` (`compressArgs`) and the READMEs.
- **Agent skill:** [skills/rootsqz-report/SKILL.md](skills/rootsqz-report/SKILL.md) tells the coding agents of rootsqz users how to make and read the compression report; `rootsqz install-skill` installs it, and `src/report/guide.rs` writes the `AGENTS.md` of each report. Keep them up to date with changes to the report, its options and `rootsqz report`.
- **Commits:** one logical change per commit, with a message that says what changed and why.
