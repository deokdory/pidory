# Contributing to pidory

## Contribution Policy

**pidory is not currently accepting external pull requests.**

This is a personal hobby project maintained by a single owner. External code contributions cannot be reviewed and merged at this time — not because your ideas aren't welcome, but because the review bandwidth simply isn't there.

**Bug reports and feature requests are very welcome:**

- File issues at https://github.com/deokdory/pidory/issues — please search existing issues before opening a new one.
- Include as much context as possible: pidory version, environment (OS, Discord setup), reproduction steps, and relevant logs or error messages.

This policy may change in the future as the project evolves. Until then, issue-based feedback is genuinely the most useful way to help.

The Branch Policy and Conventional Commits sections below are project-internal conventions enforced by git hooks — documented here for maintainers and any future contributors.

## Branch Policy

| Branch | Purpose | Direct push |
|---|---|---|
| `master` | Stable release — tagged with version only | Forbidden |
| `develop` | Integration branch — target for all PRs | Maintainer only |
| `<issue#>-<slug>` | Feature / fix branches | Contributor |

- PRs must target `develop`, not `master`.
- `master` is updated by maintainers via release merge from `develop`; it is never a direct PR target.
- Branch names must match `<number>-<slug>` format (enforced by a local git hook in this repo).

## Conventional Commits

This project uses a project-specific variant of Conventional Commits with an issue prefix:

```
#<issue> <type>: <description>
```

The `#<issue>` prefix is required for every commit (not just PR merges) and lets the issue tracker auto-link references. PR merge commits append `(#<PR>)` automatically: `#<issue> <type>: <description> (#<PR>)`.

Examples:
- `#42 feat: add /sleep command`
- `#107 fix: prevent duplicate permission prompts`
- `#231 refactor: extract parser into separate module`
- `#260 chore: bump version to v0.6.8`
- `#250 docs: update README architecture section`
- `#175 test: add unit tests for formatter split_message`

Allowed types: `feat`, `fix`, `refactor`, `chore`, `docs`, `test`.

Korean descriptions are welcome — keep the English type prefix and the `#<issue>` number at the start:

```
#42 feat: /sleep 커맨드 추가 — 세션 일시 중단
#107 fix: 권한 프롬프트 중복 발생 방지
```

If a commit does not relate to a tracked issue (rare — typically only release-prep chores), the issue prefix may be omitted:

```
chore: bump version to v0.7.0
```
