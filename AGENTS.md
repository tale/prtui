## General rules

`prtui` is a PR reviewing TUI, primarily built for GitHub but can support other
platforms as well. Here's a few important rules to adhere to when building here:

- Feature changes and bug fixes necessitate a changeset in `.changeset/` since
  we use Knope to roll them into a changelog on release.
- Crates are separated by concerns, additional platforms are new crates.
- Value simplicity, if we can do something incrementally that is appropriate. An
  example is that we used to fully utilize the `gh` CLI for GitHub, but it's
  slowly being replaced with direct API calls over time.
- Core model logic or shared functionality belongs in `prtui-core` and should be
  justified. Do not let the core turn into spaghetti.
- Shared logic across platforms (ie. a GraphQL client for the Github and GitLab
  crates) shouldn't be extracted until there is atleast 2 consumers.

Generally adhere to the current code style, prefer simplicity, self commenting
code, and avoid over engineering. We can always abstract later, incremental work
is easier to reason about and review.

## Changelog writing

- State the resulting behavior in a single declarative and concise sentence.
- Lead with the feature or the behavior that is affected.
- Be neutral, factual, and avoid subjective language.
- Try and avoid passive voice unless it genuinely reads easier.
- Don't justify or leak implementation details/unchanged behavior.
- Example: "GitHub pull requests are now fetched via the GitHub GraphQL API."
