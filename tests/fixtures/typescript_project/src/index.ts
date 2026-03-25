/**
 * Main barrel export — application entry point.
 * Demonstrates: top-level barrel exports aggregating all submodules.
 */

// Type exports
export type {
  User,
  CreateUserInput,
  UpdateUserInput,
  UserId,
  UserWithoutDates,
  PublicUser,
  ApiResponse,
  ApiError,
  ApiResult,
  PaginatedResponse,
} from "./types/index.js";

export { UserRole } from "./types/index.js";

// Model exports
export { BaseModel, UserModel } from "./models/index.js";
export type { Serializable, Identifiable, ModelId } from "./models/index.js";

// Service exports
export { UserService, NotificationService, NotificationChannel } from "./services/index.js";
export type {
  NotificationPayload,
  NotificationResult,
  NotificationProvider,
} from "./services/index.js";

// Error exports
export {
  AppError,
  NotFoundError,
  ValidationError,
  ConflictError,
  UnauthorizedError,
  isAppError,
  toApiErrorResponse,
} from "./errors/index.js";

// Utility exports
export { logger, createChildLogger } from "./utils/index.js";
export {
  createUserSchema,
  updateUserSchema,
  paginationSchema,
  validateOrThrow,
} from "./utils/index.js";
