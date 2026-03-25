// Sample: barrel export and re-export patterns
// Expected detections: barrel exports, named re-exports, type-only re-exports

// Named re-exports from submodules
export { UserService } from "./services/user.service.js";
export { NotificationService } from "./services/notification.service.js";

// Type-only re-exports
export type { User, CreateUserInput, UpdateUserInput } from "./types/user.js";
export type { ApiResponse, ApiError, PaginatedResponse } from "./types/api.js";

// Mixed re-exports (values and types together)
export {
  AppError,
  NotFoundError,
  ValidationError,
  type ConflictError,
} from "./errors/app-error.js";

// Default re-export
export { default as Button } from "./components/Button.js";

// Aggregated namespace re-export
export * as models from "./models/index.js";

// Enum re-export (value export)
export { UserRole } from "./types/user.js";
