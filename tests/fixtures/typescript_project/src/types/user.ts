/**
 * Core user-related type definitions.
 * Demonstrates: interfaces, type aliases, enums, readonly properties.
 */

export enum UserRole {
  Admin = "admin",
  Editor = "editor",
  Viewer = "viewer",
}

export interface User {
  readonly id: string;
  name: string;
  email: string;
  role: UserRole;
  createdAt: Date;
  updatedAt: Date;
  metadata?: Record<string, unknown>;
}

export interface CreateUserInput {
  name: string;
  email: string;
  role?: UserRole;
}

export interface UpdateUserInput {
  name?: string;
  email?: string;
  role?: UserRole;
  metadata?: Record<string, unknown>;
}

export type UserId = string;

export type UserWithoutDates = Omit<User, "createdAt" | "updatedAt">;

export type PublicUser = Pick<User, "id" | "name" | "role">;
