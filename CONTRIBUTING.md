# Contributing to pidory

Thank you for your interest in contributing to pidory!

## Code of Conduct

This project follows the [Contributor Covenant v2.1](https://www.contributor-covenant.org/version/2/1/code_of_conduct/).
All participants are expected to maintain a harassment-free experience for everyone, regardless of age, body size, disability, ethnicity, gender identity, level of experience, nationality, personal appearance, race, religion, or sexual identity and orientation.
Violations may be reported to the maintainers at deokdory@gmail.com.

## How to Contribute

1. **Open an issue** — Search existing issues first. If none match, open a new one describing the bug or feature request.
2. **Fork and branch** — Fork the repository and create a branch named `<issue#>-<short-slug>` (e.g., `42-fix-permission-cache`). This naming is enforced by a git hook.
3. **Write code** — Keep changes focused on the issue scope. Run `cargo clippy` and ensure there are no new warnings. Do not run `cargo fmt` globally — format only files you touch.
4. **Open a PR to `develop`** — All feature and fix PRs target the `develop` branch. Fill in the PR template and link the related issue.
5. **Review and merge** — A maintainer will review the PR. Address feedback and push updates to the same branch.

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

Commit messages follow the pattern:

```
<type>: <description>
```

Examples:
- `feat: add /sleep command`
- `fix: prevent duplicate permission prompts`
- `refactor: extract parser into separate module`
- `chore: bump version to v0.6.8`
- `docs: update README architecture section`
- `test: add unit tests for formatter split_message`

PR merge commits use the format `#<issue> <type>: <description> (#<pr>)`.

Allowed types: `feat`, `fix`, `refactor`, `chore`, `docs`, `test`.

Korean descriptions are welcome — keep the English type prefix:

```
feat: /sleep 커맨드 추가 — 세션 일시 중단
fix: 권한 프롬프트 중복 발생 방지
```
