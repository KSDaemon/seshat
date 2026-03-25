/**
 * User model implementation.
 * Demonstrates: class extending abstract base, type-only imports, method patterns.
 */

import type { User, CreateUserInput, UserRole } from "../types/index.js";

import { BaseModel } from "./base.model.js";
import { UserRole as UserRoleEnum } from "../types/index.js";

export class UserModel extends BaseModel implements User {
  public name: string;
  public email: string;
  public role: UserRole;
  public metadata?: Record<string, unknown>;

  private constructor(
    name: string,
    email: string,
    role: UserRole,
    id?: string,
  ) {
    super(id);
    this.name = name;
    this.email = email;
    this.role = role;
  }

  public static create(input: CreateUserInput): UserModel {
    return new UserModel(
      input.name,
      input.email,
      input.role ?? UserRoleEnum.Viewer,
    );
  }

  public static fromData(data: User): UserModel {
    const user = new UserModel(data.name, data.email, data.role, data.id);
    user.metadata = data.metadata;
    return user;
  }

  public updateName(name: string): void {
    this.name = name;
    this.markUpdated();
  }

  public updateEmail(email: string): void {
    this.email = email;
    this.markUpdated();
  }

  public updateRole(role: UserRole): void {
    this.role = role;
    this.markUpdated();
  }

  public isAdmin(): boolean {
    return this.role === UserRoleEnum.Admin;
  }

  public toJSON(): Record<string, unknown> {
    return {
      id: this.id,
      name: this.name,
      email: this.email,
      role: this.role,
      createdAt: this.createdAt.toISOString(),
      updatedAt: this.updatedAt.toISOString(),
      metadata: this.metadata,
    };
  }
}

export const __all__ = ["UserModel"] as const;
