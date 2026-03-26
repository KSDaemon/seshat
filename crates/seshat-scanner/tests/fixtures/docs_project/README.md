# Project Name

A brief description of the project.

## Architecture

The system uses a layered architecture:

- Core layer handles domain logic
- Storage layer manages persistence
- Scanner layer processes source files
- Graph layer builds the knowledge graph

## Getting Started

1. Clone the repository
2. Run `cargo build`
3. Execute tests with `cargo test`

### Prerequisites

- Rust 1.85 or later
- SQLite 3.35+

## API Conventions

- All endpoints return JSON
- Authentication uses Bearer tokens
* Error responses include a `message` field
