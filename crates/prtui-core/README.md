# prtui core

`prtui-core` is the contract between a code-review provider and the TUI. It
contains the complete provider-neutral view model and the `Provider` trait.

To add a provider:

1. Create a workspace crate such as `crates/prtui-gitlab`.
2. Keep authentication, transport, pagination, URLs, and wire DTOs inside it.
3. Convert every response into `prtui_core` models before returning it.
4. Implement `Provider` on a cheap-to-copy client value. Its return-position
   futures remain concrete, so generic callers use static dispatch without
   boxed futures or trait objects.
5. Add one variant to `ProviderChoice` and one arm to the startup match in
   `src/main.rs`.

Provider identifiers are opaque strings. Do not expose API-specific ID types,
JSON values, URL shapes, or transport errors to `prtui-tui`.
