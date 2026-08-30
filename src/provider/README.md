# Adding a provider

Use [`github/`](github/) as the reference implementation.

1. Create `src/provider/<name>/mod.rs` with a small client struct that
   implements the `Provider` trait from [`mod.rs`](mod.rs).
   Keep authentication, API URLs, pagination, and provider-specific errors in
   that module.
2. Convert API responses into the shared types from `provider/mod.rs`. Keep raw
   request and response schemas in the provider's own `wire.rs` rather than
   exposing them to the runtime or application model.
3. Keep provider consumers generic over `Provider`. The trait returns concrete
   `Send` futures, so calls use static dispatch without boxed futures, trait
   objects, or provider `Arc`s.
4. Add provider selection where `GitHub` is currently chosen in `main.rs`.
   If selection happens at runtime, put the choices in a small enum that also
   implements `Provider`; keep repository parsing and browser URLs behind the
   trait as well.
5. Test wire conversion inside the provider module and add one boundary test
   that parses a repository and builds its web URL through the trait.

Provider clients should be cheap to copy. Put reusable connection pools or
credential caches behind their transport module instead of copying them into
each client value.
