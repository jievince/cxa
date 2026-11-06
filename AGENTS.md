# Repository workflow

- After each requested cxa change is implemented and verified, commit the related changes and push them to the personal fork at `origin` (`https://github.com/jievince/cxa.git`). This is a standing user instruction. Do not push to `upstream`.
- Test with isolated `HOME`, `CODEX_HOME`, and `CXA_ACCOUNT_STORE` directories. Never test account switching against the user's real Codex credentials or configuration.
- Names remain the unique account identifiers; partial matching is not supported.
- Switching requires `cxa use NAME` or `cxa use N`; do not reintroduce bare account shorthand.
- Lists show consecutive positions starting at 1. Only `use` accepts these numbers, resolving them to names in the current list order under the store lock. Positions are never stored identities or profile slots; other account commands accept names only.
