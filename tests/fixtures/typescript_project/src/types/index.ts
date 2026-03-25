/**
 * Barrel export for types module.
 * Demonstrates: re-exporting from submodules, type-only re-exports.
 */

export {
  UserRole,
  type User,
  type CreateUserInput,
  type UpdateUserInput,
  type UserId,
  type UserWithoutDates,
  type PublicUser,
} from "./user.js";

export type {
  ApiResponse,
  ApiError,
  ApiResult,
  PaginatedResponse,
  GetUserParams,
  ListUsersParams,
  CreateUserRequest,
  UpdateUserRequest,
  GetUserResponse,
  ListUsersResponse,
} from "./api.js";
