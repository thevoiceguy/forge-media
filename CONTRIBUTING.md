# Contributing to Forge Media Engine

Thank you for your interest in contributing to Forge! This document provides guidelines and information for contributors.

---

## 🌟 How to Contribute

### Reporting Issues

- Check if the issue already exists
- Use the issue template
- Provide clear reproduction steps
- Include relevant logs and system information

### Suggesting Features

- Open an issue with the `enhancement` label
- Describe the use case and benefits
- Consider implementation complexity
- Be open to discussion and feedback

### Code Contributions

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Make your changes
4. Add tests for new functionality
5. Ensure all tests pass (`cargo test`)
6. Format your code (`cargo fmt`)
7. Run clippy (`cargo clippy`)
8. Commit your changes (`git commit -m 'Add amazing feature'`)
9. Push to your branch (`git push origin feature/amazing-feature`)
10. Open a Pull Request

---

## 🛠️ Development Setup

### Prerequisites

- Rust 1.75 or later (install via [rustup](https://rustup.rs/))
- C compiler (GCC, Clang, or MSVC)
- OpenSSL development libraries

### Recommended Tools

```bash
# Code formatting
rustup component add rustfmt

# Linting
rustup component add clippy

# Development utilities
cargo install cargo-watch    # Watch files and run commands
cargo install cargo-edit     # Add/remove/upgrade dependencies
cargo install cargo-nextest  # Next-generation test runner
cargo install cargo-expand   # Expand macros
```

### Building

```bash
# Debug build (fast compile, slower runtime)
cargo build

# Release build (slower compile, fast runtime)
cargo build --release

# Build specific crate
cargo build -p forge-core

# Build with all features
cargo build --features full
```

### Running Tests

```bash
# Run all tests
cargo test

# Run tests for specific crate
cargo test -p forge-rtp

# Run with output
cargo test -- --nocapture

# Run with logging
RUST_LOG=debug cargo test

# Run integration tests
cargo test --test '*'

# Use nextest (faster)
cargo nextest run
```

### Code Quality

```bash
# Format code
cargo fmt

# Check formatting without modifying
cargo fmt -- --check

# Run clippy
cargo clippy

# Run clippy with all features and strict warnings
cargo clippy --all-features -- -D warnings

# Check for unused dependencies
cargo udeps
```

---

## 📐 Code Style

### Naming Conventions

- **Crates**: `forge-{feature}` (e.g., `forge-rtp`)
- **Structs**: `PascalCase` with suffix:
  - `{Feature}Manager` for managers
  - `{Feature}Config` for configuration
  - `{Feature}Error` for errors
  - `{Feature}Event` for events
- **Functions**: `snake_case`
- **Constants**: `SCREAMING_SNAKE_CASE`

### Code Organization

```rust
// Module structure
pub mod types;    // Public types
pub mod config;   // Configuration
pub mod error;    // Error types
mod internal;     // Private implementation

// File structure
use std::...;     // Standard library
use tokio::...;   // External crates
use forge_core::...; // Internal crates
use crate::...;   // Local imports

// Public API first, private second
pub struct Public { }
impl Public { }

struct Private { }
impl Private { }
```

### Error Handling

Use `thiserror` for error types:

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MyError {
    #[error("Something went wrong: {0}")]
    Failed(String),

    #[error("IO error")]
    Io(#[from] std::io::Error),
}
```

### Async Patterns

```rust
// Prefer async/await
pub async fn do_something() -> Result<()> {
    let result = async_operation().await?;
    Ok(())
}

// Use spawn for independent tasks
tokio::spawn(async move {
    // Independent task
});

// Use try_join for concurrent operations
let (a, b) = tokio::try_join!(
    fetch_a(),
    fetch_b()
)?;
```

### Documentation

```rust
/// Brief description (one line)
///
/// Longer description with details, examples, and notes.
///
/// # Arguments
///
/// * `param` - Description of parameter
///
/// # Returns
///
/// Description of return value
///
/// # Errors
///
/// Description of error conditions
///
/// # Examples
///
/// ```
/// let result = function(param);
/// assert_eq!(result, expected);
/// ```
pub fn function(param: Type) -> Result<ReturnType> {
    // Implementation
}
```

---

## 🧪 Testing Guidelines

### Unit Tests

Place in same file as code:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_functionality() {
        let result = function();
        assert_eq!(result, expected);
    }

    #[tokio::test]
    async fn test_async_functionality() {
        let result = async_function().await.unwrap();
        assert_eq!(result, expected);
    }
}
```

### Integration Tests

Place in `tests/` directory:

```rust
// tests/integration/basic_call.rs
use forge_engine::ForgeEngine;

#[tokio::test]
async fn test_basic_call_flow() {
    let engine = ForgeEngine::new(test_config()).await.unwrap();
    // Test implementation
}
```

### Test Data

Use the builder pattern for test data:

```rust
fn test_config() -> ForgeConfig {
    ForgeConfig {
        engine: EngineConfig {
            port_range: PortRange { start: 40000, end: 41000 },
            ..Default::default()
        },
        ..Default::default()
    }
}
```

---

## 📋 Pull Request Guidelines

### PR Checklist

- [ ] Code follows style guidelines
- [ ] Tests added for new functionality
- [ ] All tests pass (`cargo test`)
- [ ] Documentation updated
- [ ] CHANGELOG.md updated
- [ ] No clippy warnings (`cargo clippy`)
- [ ] Code formatted (`cargo fmt`)

### PR Description Template

```markdown
## Description
Brief description of changes

## Type of Change
- [ ] Bug fix (non-breaking)
- [ ] New feature (non-breaking)
- [ ] Breaking change
- [ ] Documentation update

## Testing
How has this been tested?

## Checklist
- [ ] Tests added/updated
- [ ] Documentation updated
- [ ] No clippy warnings
```

### Review Process

1. Automated checks must pass (CI)
2. At least one maintainer approval required
3. Address review feedback
4. Squash commits if requested
5. Maintainer will merge

---

## 🏗️ Architecture Guidelines

### Adding New Features

1. Review [DEVELOPMENT_PLAN.md](DEVELOPMENT_PLAN.md) for phase alignment
2. Discuss design in issue first
3. Create new crate if substantial (>1000 LOC)
4. Update documentation

### API Design

- Version all APIs (`/v1/...`)
- Use RESTful conventions
- Return consistent error format:
```json
{
  "status": "error",
  "error": {
    "code": "session_not_found",
    "message": "Session abc-123 not found"
  }
}
```

### Performance Considerations

- Profile before optimizing
- Avoid allocations in hot paths
- Use `Arc` for shared data
- Prefer channels over locks for communication
- Consider zero-copy techniques

---

## 📚 Resources

### Rust Resources
- [The Rust Book](https://doc.rust-lang.org/book/)
- [Rust Async Book](https://rust-lang.github.io/async-book/)
- [Tokio Tutorial](https://tokio.rs/tokio/tutorial)

### RTP/SIP Resources
- [RFC 3550 - RTP](https://www.rfc-editor.org/rfc/rfc3550)
- [RFC 3711 - SRTP](https://www.rfc-editor.org/rfc/rfc3711)
- [RFC 3261 - SIP](https://www.rfc-editor.org/rfc/rfc3261)

### Project Documentation
- [Architecture](FORGE%20ARCHITECTURE.md)
- [Development Plan](DEVELOPMENT_PLAN.md)
- [Claude Guide](CLAUDE.MD)

---

## 💬 Communication

- **GitHub Issues**: Bug reports, feature requests
- **Pull Requests**: Code review, discussion
- **Discussions**: General questions, ideas

---

## 🎯 Good First Issues

Look for issues labeled `good first issue` - these are great entry points for new contributors.

---

## ❓ Questions?

Don't hesitate to ask! Open an issue with the `question` label.

---

**Thank you for contributing to Forge!** 🔨
