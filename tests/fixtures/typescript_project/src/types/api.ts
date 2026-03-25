/**
 * API request/response type definitions.
 * Demonstrates: type-only imports, generic types, discriminated unions.
 */

import type { User, UserId, CreateUserInput, UpdateUserInput } from "./user.js";

export interface ApiResponse<T> {
  success: boolean;
  data: T;
  timestamp: string;
}

export interface ApiError {
  success: false;
  error: {
    code: string;
    message: string;
    details?: unknown;
  };
  timestamp: string;
}

export type ApiResult<T> = ApiResponse<T> | ApiError;

export interface PaginatedResponse<T> {
  items: T[];
  total: number;
  page: number;
  pageSize: number;
  hasMore: boolean;
}

export interface GetUserParams {
  id: UserId;
}

export interface ListUsersParams {
  page?: number;
  pageSize?: number;
  role?: string;
}

export type CreateUserRequest = CreateUserInput;
export type UpdateUserRequest = UpdateUserInput & { id: UserId };

export type GetUserResponse = ApiResponse<User>;
export type ListUsersResponse = ApiResponse<PaginatedResponse<User>>;
