## Seshat — Project Convention Intelligence

**MANDATORY.** Seshat maintains this project's conventions, patterns, and
architectural decisions. You MUST query it BEFORE writing or modifying code.

### Before Any Code Action

| Trigger | Mandatory Tool Call |
|---------|---------------------|
| Starting any session | `query_project_context()` |
| Writing a new function, class, module, type | `query_code_pattern(query="<name>")` |
| Choosing patterns or conventions | `query_convention(topic="<area>")` |
| Implementing any feature, fix, or refactor | `validate_approach(description="<plan>")` |
| Editing any existing file | `query_dependencies(path="<file>")` |
| Discovering a new pattern or decision | `record_decision(description="<what>", reason="<why>")` |

`topic` can be anything — e.g. `"error handling"`, `"database"`, `"api design"`, `"logging"`, `"async"`, `"naming"`, `"testing"`, `"migrations"`, `"state management"`...

### Rules

- NEVER write or modify code without first calling `validate_approach`
- NEVER create a new function/type/module without first calling `query_code_pattern`
- NEVER edit a file without checking `query_dependencies`

Load the `seshat` skill for full workflow and examples.
