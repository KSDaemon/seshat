/**
 * User service — business logic for user operations.
 * Demonstrates: ESM imports, type-only imports, class-based service, async patterns.
 */

import type { User, CreateUserInput, UpdateUserInput, UserId } from "../types/index.js";

import { UserModel } from "../models/index.js";
import { NotFoundError, ConflictError } from "../errors/index.js";
import { logger } from "../utils/logger.js";

export class UserService {
  private readonly users: Map<string, UserModel> = new Map();

  public async createUser(input: CreateUserInput): Promise<User> {
    logger.info(`Creating user: ${input.email}`);

    const existing = this.findByEmail(input.email);
    if (existing) {
      throw new ConflictError(`User with email '${input.email}' already exists`);
    }

    const user = UserModel.create(input);
    this.users.set(user.id, user);

    logger.info(`User created: ${user.id}`);
    return user.toJSON() as unknown as User;
  }

  public async getUserById(id: UserId): Promise<User> {
    const user = this.users.get(id);
    if (!user) {
      throw new NotFoundError("User", id);
    }
    return user.toJSON() as unknown as User;
  }

  public async updateUser(id: UserId, input: UpdateUserInput): Promise<User> {
    const user = this.users.get(id);
    if (!user) {
      throw new NotFoundError("User", id);
    }

    if (input.name !== undefined) {
      user.updateName(input.name);
    }
    if (input.email !== undefined) {
      const emailConflict = this.findByEmail(input.email);
      if (emailConflict && emailConflict.id !== id) {
        throw new ConflictError(`Email '${input.email}' is already in use`);
      }
      user.updateEmail(input.email);
    }
    if (input.role !== undefined) {
      user.updateRole(input.role);
    }

    logger.info(`User updated: ${id}`);
    return user.toJSON() as unknown as User;
  }

  public async deleteUser(id: UserId): Promise<void> {
    const user = this.users.get(id);
    if (!user) {
      throw new NotFoundError("User", id);
    }

    this.users.delete(id);
    logger.info(`User deleted: ${id}`);
  }

  public async listUsers(page: number = 1, pageSize: number = 20): Promise<{
    items: User[];
    total: number;
  }> {
    const allUsers = Array.from(this.users.values());
    const start = (page - 1) * pageSize;
    const items = allUsers
      .slice(start, start + pageSize)
      .map((u) => u.toJSON() as unknown as User);

    return {
      items,
      total: allUsers.length,
    };
  }

  private findByEmail(email: string): UserModel | undefined {
    for (const user of this.users.values()) {
      if (user.email === email) {
        return user;
      }
    }
    return undefined;
  }
}
