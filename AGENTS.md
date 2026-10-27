# Development rules

Read the README and source interface documentation before changing behavior.
Keep one crate organized by native subsystem. Applications own scene structure,
shader generation and resource policy; this library owns SDK handles and completion.

Document public interfaces in English beside their declarations. Preserve thread
affinity, cancellation, allocation ownership and native error codes. Keep real-time
audio free of allocation and file IO. Validate native contracts at their boundaries.

Rust implementation files stay within 1,500 lines, or 2,000 with tests. Unit tests
live with their owning module; integration tests live under tests/. Keep generated
artifacts under ignored target/ or temporary storage. Preserve reproducible focused
checks, and identify target-only behavior that still needs device validation.
