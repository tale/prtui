# Adding a provider

Use [`github/`](github/) as the reference implementation.

1. Create `src/provider/<name>/mod.rs` with a small client struct and inherent
   methods matching the operations forwarded by `Provider` in [`mod.rs`](mod.rs).
   Keep authentication, API URLs, pagination, and provider-specific errors in
   that module.
2. Convert API responses into the shared types from `provider/mod.rs`. Keep raw
   request and response schemas in the provider's own `wire.rs` rather than
   exposing them to the runtime or application model.
3. Add the module and a variant to `Provider`, then forward every `Provider`
   method to the new client. `Provider` intentionally uses concrete enum
   dispatch: do not add boxed futures, trait objects, or provider `Arc`s.
4. Add provider selection where `Provider::github()` is currently chosen in
   `main.rs`. Repository parsing and browser URLs must also go through the
   selected provider.
5. Test wire conversion inside the provider module and add one boundary test
   that parses a repository and builds its web URL through `Provider`.

Provider clients should be cheap to copy. Put reusable connection pools or
credential caches behind their transport module instead of copying them into
each client value.
