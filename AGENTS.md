# Repository workflow

- After each requested cxa change is implemented and verified, commit the related changes and push them to the personal fork at `origin` (`https://github.com/jievince/cxa.git`). This is a standing user instruction. Do not push to `upstream`.
- Test with isolated `HOME`, `CODEX_HOME`, and `CXA_ACCOUNT_STORE` directories. Never test account switching against the user's real Codex credentials or configuration.
- Accounts are selected only by their unique names. Do not reintroduce numeric selectors or partial matching.
